# Replication and archives

The long form of the invariants [crates/fennec-http/CLAUDE.md](../../crates/fennec-http/CLAUDE.md) and [crates/fennec-import/CLAUDE.md](../../crates/fennec-import/CLAUDE.md) list under *Replication and archives*: replication, a write's time on a copy and in an archive, a node's tenants archived over one stream, `/_changes`, `Fennec-After`, sealed archives and `--follow`. Each section below is a paragraph moved word for word from CLAUDE.md, which keeps its rule in a few sentences and links here; "above" and "below" in them refer to the order the invariants had there, which [README.md](README.md#the-original-order) lists.

## Replication

**Replication ships only what is on disk, numbered by the change counter.**
A primary (`--replication-token`) writes through a `Tee`
(`fennec-http/src/replication.rs`): a write's record enters a bounded feed in
memory as it is appended, and is sent once an fsync has covered it -- so a
primary back from a crash holds every write any replica was sent. A replica
applies them with `Database::apply`, which does the write path's index upkeep
and hands each record on to its own sink with `Sink::record(seq, ..)`: its
change counter matches the primary's write for write, and its file reopens
where it stopped. A record is handed on as a block lands, whole in memory
first, and one that fails or a panic stops part way is put back whole,
with every record after it not landed, never half of it served; the follower catches the panic with the lock
held, as a write it could not apply, and connects again for an image --
or stops, where the panic came from the sink call on and failed the copy
([engine.md](engine.md#blocks)). So every write goes through `wal`, and a record moves the
counter by the writes it holds (a block's, `writes_in`); a record that moved
no counter would leave every replica one change off. The
history (record kind 8, `History`) moves none, as a graph a server keeps in
the tail does, and neither is sent. A promotion forks the history, a replica is continued only from a position
the primary's history passed through and sent an image otherwise, and a
following database refuses writes (`Error::ReadOnly`, 403). A promotion
halts the follower before it forks, and a follower reconnecting to a
primary that is gone walks away from the dial at the halt
(`fennec_http::dial::connect_unless`: the lookup and the connect on a
thread of their own, the stop flag looked at every 10 ms, the stream
handed to `halt` before its request goes out): waited for, the halt took
the connect's 5 s where the dead machine's address swallowed packets, and
as long as the resolver took to refuse its name -- the tail of a failover
in V5's drill on CI; `tests/replication.rs` holds a promotion beside a
lookup that never ends (30 s on main, the 5 s of the dial's bound with the
dial not walked away from) and beside a primary that takes the connection
and never answers (the follower's 10 s of silence where the stream reached
`halt` only with the head). The session looks at the halt before each
message it reads (`Follower::session`), as an archiver's looks at its
stop (`archive.rs`, `node_archive.rs`), and never at the socket's cut
alone: the cut is a `shutdown` through a duplicate of the socket
(`try_clone`), which on Unix shares the socket, every read after it
reading its end, and on Windows (`WSADuplicateSocket`) ends a read already
waiting but not one begun after it -- that read reads on, the primary's
keep-alive every 2 s (`ALIVE`) keeping it short of its 10 s of silence. A
follower between two reads at the halt -- landing a write, its fsync under
`--sync always`, or not yet back at its read -- so followed on for good,
and the halt waited for it for good: five runs of `windows (http)` between
2026-11-08 and 11-13 hung until cancelled, 1 to 6 hours each, every one in
a test halting a follower just after a write landed on it
(`a_stream_and_changes_sent_after_a_write_wait_for_it_on_a_replica`, which
halts three times; a restarted replica; a compaction beside a replica; a
copy taking an image) or promoting one
(`node_archive::a_promoted_tenant_is_archived_on_its_new_history_without_an_image`).
On Unix it never showed: of 4 500 runs of the first under load, on macOS
and in Docker on Linux, none hung. Now a halt between reads ends the
session at once, and one whose look came just before the cut ends it after
the next message: a keep-alive at most 2 s on from a live primary, or
the session's 10 s of silence (`SILENCE`) from a stalled one. `tests/replication.rs`'
`a_halt_ends_a_follower_held_between_two_reads` holds a follower in its
hello's callback, past its read, until the cut has reached the primary's
side of a relay between them, then lets it go; with a cut that leaves later
reads alone, as Windows' does (on macOS, `Shutdown::Write` in its place),
the loop as it was read on and the test failed at its 20 s, the hung test
failing at its first halt, and fennec-http's whole suite hung where
Windows' had, in `node_archive`'s promotion (and in `dials`' replica over
HTTPS, halted as it follows). With the loop looking at the flag every one
passes under that stand-in but the promotion beside a primary that never
answers, which asks the cut to end a read already waiting: Windows' does,
the test passing there, and `Shutdown::Write` does not. The tests' halts
are bounded (`halt`, 20 s) and a stream's first event is read within 30 s
as a whole (`first_event`), so a hang fails in seconds naming what did not
end. Rejected: cancelling the read on Windows
at each turn of the halt's wait (`CancelIoEx`, as `conn::end_all` ends a
TLS connection's) -- Windows' own call, behind `tls` today, for a read the
flag ends within one message; and a read timeout under `ALIVE` -- a wake
for every follower, every second, to look at a flag the primary's next
message brings it to anyway. A durable
write is answered once every stream has written it into its socket
(`Feed::wait_sent` in the `Tee`'s durability, `SENT_WAIT` a second at
most, a stream that missed it `lagging` and not waited for until it
catches up): sent after the answer, a write answered in the moment before
was lost when the primary died -- a node of the SaaS example killed under
eight writers lost one in 3 of 60 runs on main, none in 120 since -- and
a socket's bytes reach the replica after the process is killed, though
not after the machine dies. Lag is 0.04 ms p50 under `--sync always`
(0.14 before the wait) and at most 283 ms under `--sync 250`, durable
writes 254 a second against 246; ten failovers under `always` lost no
acknowledged write, each promoting a replica that kept up. That is the
promise and all of it: none the replica was sent. A replica lagging,
reconnecting or failed is waited for not at all, and a promotion asks no
position, so one not connected, or behind, as its primary is lost is
promoted where it stands, without the writes its primary answered
meanwhile -- and on a plain-HTTP hop whoever can reach it can cut the
stream and hold it there. It is watched (`connected`, `behind` and
`last_contact_ms` in a replica's status, `fennec_replica_*` and a node's
`fennec_copies_*` in `/_metrics`, the kit's alerts on each) and a
tenant's promotion says what its copy knew ([tenants.md](tenants.md),
`replication::Heard`); the site said a failover under `--sync always`
lost none, from 0.1.5, until the review's second round (G14, area D).
"Sent" is handed to the primary's kernel, which a machine lost takes
with it; and a stream sent an image is waited for from the image's flush
(`sending_standby`), so the writes answered between it and the replica's
adoption of the image -- its check and load, which a halt walks away
from -- are lost to a promotion then, the copy saying `connected` false.
`behind` counts from what the primary last said of its disk: a keep-alive
every `ALIVE` (2 s), which a stream sent only once it had caught up, so a
replica as far behind as the feed's buffer -- its reads slower than the
writes, its socket full -- read `behind` 0, and so did a node's
`fennec_copies_behind`, a promotion's answer and the router's line, and
`FennecTenantCopiesBehind` never fired on it. A stream behind now sends
one after the writes it sends once `ALIVE` has passed since it last told
(`stream`'s `told`; the review's third round): 33 bytes every 2 s a
lagging stream, a stream that has sent every write on disk saying
nothing. It looks, at the clock once a `W` message and at the feed's
`durable` once `ALIVE` has passed, after `Feed::sent` has let the writes
waiting on that `W` go, and sends the keep-alive in a flush of its own --
a `write` every 2 s, none a `W` -- so a `--sync always` write waits for
neither: nothing on a request's path. The third round sent it in the
`W`'s flush, where such a write waited on the look and, every 2 s, on the
keep-alive's bytes; the fourth moved both after `Feed::sent`. Every
replica since 0.1.5 reads a keep-alive between any two messages, and an
archiver syncs its segment on one, as it does every
250 ms while it takes writes. `connected` is true until a read fails: a
primary's process killed ends the stream at once, its machine lost or cut
off only after `SILENCE` (10 s) -- past a 5 s lease's lapse -- so a
router reads `last_contact_ms` beside it ([tenants.md](tenants.md)). An archive (`fennec archive`,
`fennec-http/src/archive.rs`) is the same stream written to files, each write
with the time the primary appended it -- an archive of a copy too, [below](#a-writes-time); `fennec restore` is an image plus the
archived writes up to a time or a change, forked -- a fennecdb file is exactly
that, so a restore is a concatenation checked by opening it. An archive
takes images of its own end (`Archive::consolidate`, `--image-every`, an
hour): it kept its first image for good, and a restore replayed every write
since. The segment being written ends at the next write after one, so
`prune` (`--keep`) lets go of whole segments -- in the open segment the
writes before an image stayed until it reached 64 MB. `fennec verify` reads
an archive as a restore would. A segment is closed and fsynced before the
next begins and an image renamed into place, so a sync tool's copy, taken
a file at a time while the archive writes, is the archive up to a moment:
fennecdb speaks no S3, and S3 and R2 are reached with `rclone`.

*Over TLS.* A primary serving HTTPS (`--tls-cert`) is followed at
`https://host:port` (`--replica-of`, a tenant's follow, `fennec backup`
and `archive`): the stream is the same bytes in a TLS session
(`dial::Stream`, [http.md](http.md#dialing-https)), the primary's
certificate held to `--tls-ca` or Mozilla's roots before the request -- the
token in it -- goes out. The dial's handshake is inside `CONNECT`'s 5 s and
on `connect_unless`'s thread, so a halt still walks away from it; the
socket beneath the session is what `halt` shuts. Nothing the primary does
changes: `Feed::wait_sent` waits for a stream to have written a write into
its connection, which over TLS is into the session's records and their
send (`Conn::write_or_hold`, slice 2), `SENT_WAIT` bounds it as before, and
a stream whose send would block marks itself `Sender::held` first, as
plain.

A segment is made in three calls -- the file, its magic, its first write
(`Archive::take`) -- so a copy or a crash between them finds one holding no
write, and before the magic one that is only a start of it, empty included
(sealed, of the seal's, or its head frame cut short). Such a segment holds
no write (`entries`), so a copy whose last segment it is verifies and
restores to the segment before; and the archiver opening the archive to go
on removes a last segment holding no write (`position`), the directory
synced before it goes on in the one before: brought back by a crash, the
removed one would hold no write with segments after it, which `verify`
refuses by name -- only the last segment may hold none. `consolidate` reads
the archive's end from the segment before it too (`last_write`). Refused as no segment, a cut magic made a copy "not an archive
segment" (`a_copy_taken_while_the_archive_writes_restores_or_says_why_not`,
once in CI; in a three-core container beside three busy loops 1 run of
1 300, and 0 of 1 000 since; with the window widened by a 2 ms sleep between
the file and its magic, 18 of 100 and 0 of 100), failed an image of the
archive's end taken then (`consolidate`), and kept an archiver stopped there
from going on. Left in place, a segment holding no write had the archiver go
on from its last image, taking the writes after it again into a segment of
their own, and an image sent then put the next segment after one cut short,
which no `verify` passes. Making a segment under a temporary name and
renaming it in was not taken: a rename can still leave a last segment
holding only its magic, which needs `position`'s walk back all the same, and
an archive written before needs the lenient read all the same -- two
mechanisms and an fsync for each segment made, where one rule does.

*A round's own file.* `consolidate` assembles the archive's end -- its
last image and the writes after it -- in a file beside them and takes the
database it opens to as the image. Every round assembled in one name,
`consolidating.fennec`, removed after with its failure let go of: on
Windows a copier holding it as a round ended -- a sync tool, an indexer,
`fs::copy`'s `CopyFileExW`, sharing no deletion (32) -- kept it there, the
next sync copied a stale file the size of the database, and the round
after could not write over it and took no image (#526 had the test of a
copy taken while the archive writes accept that refusal on Windows). Now
a round assembles in `consolidating-<end>.fennec`, named by the change it
reaches -- or, where a file of that name stays, the first
`consolidating-<end>-<k>.fennec` that does not (`scratch_name`) -- and
reads it back through the handle that wrote it (`assemble_into`), never
opening it again by its name: on Windows that handle writes it from its
making (`fs::copy_private`), where a copier holding it between `fs::copy` and an
open after kept the round's appends, and its read -- an open of a
database asks to write (`fs::open_in_memory`) -- from it. On a Unix the
image is cloned on APFS where it is its owner's alone and copied by
`copy_file_range` on Linux (docs/design/durability.md#files-modes), and
nothing changes but the name. What rounds left goes as a
round begins and as an archiver opens the archive to write
(`Archive::sweep`, in `writer`, never while a round runs beside it),
`consolidating.fennec` among it, and a file that will not go is said in
the log once, the rounds naming theirs past it. The copy test takes every
image on every system again, and
`a_round_consolidates_in_a_file_of_its_own_and_sweeps_what_others_left`
plants what rounds left -- on Windows one held as a copier holds it, under
the round's own name -- and finds the image taken and every one gone once
let go of (in the `windows (http)` job, the held one said in the log
once). Its code is in `fennec-server`'s 832 bytes more of `__text`
([http.md](http.md#connections-without-a-thread)).

**A restart reads a copy's role from its file.** An open makes a file
follow only where the follow asks it to (`demote`) or where it has no
history yet; otherwise a file whose history says it takes writes stays a
primary's, `.follows` or not -- a promotion whose removal of `.follows`
a crash lost must stay one ([tenants.md](tenants.md)). So the record
saying a file follows is on disk before anything counts on it. A follow
syncs it before it is answered (`Tenants::open_as`), and an image comes
with it: `Follower::session` appends the record to the image's bytes
before `adopt` writes them, fsyncs them and renames them into place, and
`follow` then appends nothing. Both records were once left in the sink's
buffer, for a write behind, which a copy writes only as it applies its
primary's writes. A copy made to follow and then waiting for its image
writes nothing, so a kill then lost the record. The restart opened the old
primary's file as a primary's, with no follower and nothing in the log,
while the router counted it the tenant's replica. `make chaos`'s catch-up
fault found this in five runs on 2026-11-13 -- CI's seeds 1003 and 2001
to 2003, a laptop's 902 -- each a node killed
within seconds of the router's repair having its old primary's files
follow, over a link throttled to 256 or 512 kbit/s
([integrations.md](integrations.md#the-chaos-drill)). An image's
record was a window only until the copy's first write applied after it.
`tenants::tests`' `a_follow_answered_has_its_file_say_it_follows` and
`tests/replication.rs`' `an_image_a_copy_took_says_on_disk_that_it_follows`
each copy the file as a kill leaves it, the page cache without the
process's buffer, and open it again; each fails without its half of the
fix. The cost is one fsync more for each follow that changes what a file
is, some 30 bytes more in an image's own rename, and 768 bytes of
`fennec-server`'s `__text` (4 818 432 to 4 819 200, 16 bytes on the file;
macOS arm64, release). The image's buffer is
grown by the record alone: a `Vec` grown its own way took twice a large
image's size.

*What a replica reads of its primary.* Whoever answers at the address a
replica follows -- or stands between them where the hop is plain HTTP --
sends what they like, and the reader of a stream (a follower, `fennec
archive`, `fennec backup`, a node's archiver) holds every byte of it to
what a primary of this code sends, refusing the rest in words; the
internal review G14 (area D, 2026-11-15) found four ways it did not. Each
is held by a stand-in at the primary's address in fennec-http's
`tests/primaries.rs`, and for a tenant node -- its read answered, its
admin's promotion completed and the node stopped on SIGTERM within 10 s
after each -- in fennec-server's.

- *A message is held to its kind's bound before its body is read, and
  grown as it comes* (`read_kind`). An image and writes are `LONGEST`, a
  terabyte: an image is the whole database, and a block's record goes
  alone however long it is -- past 16 MB a block spills into its primary's
  file, and the feed sends it whole (`landed_block`), so nothing smaller
  bounds it. A hello, a keep-alive and an end are `SHORT`, a mebibyte: a
  keep-alive is 24 bytes, and a hello's lineage an entry of 18 bytes at
  the most for each promotion its database had, some 58 000 of them. A
  node's stream of every tenant adds a name's 256 bytes to each
  (`node_sends`). A kind no primary sends is refused at its head, as it
  was at its body. The body was `vec![0; len]` for any length to a
  terabyte, allocated before a byte of it came: Linux's overcommit
  refuses an allocation past memory and swap, and std answers a refused
  one by aborting the process, so nine bytes killed a node with every
  tenant it held, and its supervisor's restart followed the same stream.
  On macOS the terabyte was mapped lazily and its read refused (EINVAL);
  the unit test's reader was asked to fill 1 099 511 627 776 bytes for 3
  MiB sent. Now a body is first given `FIRST` (2 MiB and 16 bytes) at the
  most and doubles, to its length and no further, as its bytes fill it,
  and a message cut short says so with its length. Each allocation is
  asked with `try_reserve_exact`, a refusal an error naming the kind, the
  length and the bytes asked for (`grown`): with `reserve_exact` a refused
  one still aborted a node, `fennec archive` or `fennec backup` once a
  peer sent enough bytes (the review's second round); the image's room
  for the record saying it follows is asked so too (`loaded`, the third
  round), where `reserve_exact` copied the image whole into a buffer
  aborting on a refusal. What `Database::load` allocates as it reads the
  image -- its documents, its indexes -- still aborts the process on a
  refusal: the engine has no fallible path for a load, and an image taken
  whole is a database's size by its primary's word. A growth holds the
  bytes that came and the buffer twice as long the allocator moves them
  into -- three times what came at the most -- and a message read whole
  has held twice its length at the most, not the "twice what came" this
  first said: measured by an allocator counting the reading thread's
  bytes, a growth counted as the move it may be (`tests/message_peak.rs`,
  a binary of its own), an image of 40 MiB held 72 MiB at the most, 1.80
  times it; one a byte past `FIRST` doubled twice, 2.00 times; and a
  terabyte announced with 3 MiB sent, refused as cut short, 6 MiB, three
  times the `FIRST` that had come as it grew. The follower's image then
  takes the record saying it follows, one more growth of the image's
  length (`loaded`). A stream's writes in one message where no
  record goes alone -- `MESSAGE` of records and a time for each, fewer
  bytes than its record -- are under `FIRST`, so a steady stream's message
  is still one zeroed allocation of its length and one `read_exact`, the
  code it was but a match on its kind and a comparison
  (`a_message_is_held_in_its_length_and_its_kind_bound` holds the
  capacity to the length). Past it, an image is copied as it doubles and
  zeroed as it grows -- once its length over, beside its transfer, its
  checks and its load.
- *A `W` message's count of writes is held to its bytes.* `16 + 8 * n`
  wrapped in a release build for a count of 2^61, a body of 32 bytes
  passed, and the times were collected 2^61 of them: a panic of the
  follower's thread -- in a debug build the multiplication's own -- and in
  `fennec archive`, built with `panic = abort`, the process's end. The
  check is `node_archive`'s, which had it already: `checked_mul`,
  `checked_add`, past the body refused naming the count. `fennec backup`
  reads a hello and an image alone, and met the terabyte.
- *The answer's head is held to the server's bound for a request's*
  (`MAX_HEADER_BYTES`, 64 KiB), each line read by the server's own reader
  (`http::read_line`) with what is left of it: the status line was read
  to its newline however long, and a header line measured only once its
  newline came, so a stand-in sending a line with no end, a byte in each
  `SILENCE`, grew a replica's memory for as long as it liked -- the test's
  stand-in wrote 64 MiB of a status line into one. A line two bytes at the
  least, the count of header lines is held by the bytes, as a request's
  is; a line runs a buffer's fill past the bound at the most before it
  is refused.
- *A panic anywhere in a session is that session's failure*
  (`Follower::session_caught`), caught in words, logged as a broken
  stream is (`replication: a panic in the session: ...; connecting
  again`) and in the status's `error`. It was caught around `apply`
  alone: one in the framing, an image's check and load, its adoption or
  a hello's callback ended the thread, and `ended` stayed false -- the
  line setting it was at the end of `run`, which a panic skips -- so the
  copy served what it held for good, `running` said it ran, a tenant's
  close kept it open, and a halt waited for ever: a promotion's; a
  release's, holding the tenant's slot, behind which every later `get` of
  the tenant waited, connection threads piling up to the cap, and the
  node's shutdown, which takes every slot, never completed. The thread
  now says its end as a guard's drop (`Ends`), whichever way it ends. A
  panic that changed nothing of the copy -- the framing, an image's check,
  the image loaded into a database of its own before the lock, a hello's
  callback -- is a session that failed: the follower connects again after
  its back-off. One that may have left the copy part way through a change
  the engine does not put back itself -- an image adopted (its file
  replaced, then its memory), its history followed, this server's feed
  started on it, a message's times, its flush and its fsync -- unwinds
  through `Changing`, made before the write lock so it drops after it,
  which fails the database in words (`Database::fail`: "a panic as the
  follower adopted its primary's image: the copy's file and memory may
  disagree, ..."): no write lands on the copy, a promotion's fork is
  refused with those words (500), its status's `error` names the panic,
  and the loop stops, as after its disk failed; opened again, the copy is
  its file's. A panic in `apply` is the engine's, which puts back what did
  not land or fails the database from the sink call on, as before. No
  panic of the image path is reached by a primary's bytes in a release
  build -- `check_records` and `load` are the `image` target's -- but one
  was in a debug build: a `debug_assert` that the image is at the change
  its hello said, now a refusal before the image replaces anything. The
  tests force one through a hook a database already has, a watcher whose
  `notify` panics, which `adopt` calls last, under the lock, and one
  through a hello's callback.
- *A halt waits on the thread's end as an event* (a `Condvar` `wait`; it
  looked every 50 ms), and every place the thread can be is bounded: a
  read of the primary ends at the cut -- on Unix at once, on Windows at
  once where it waited already -- and one begun after the cut ends at its
  next byte, the read looking at the halt before each read of the socket
  (`Watched`, an atomic's load beside a system call), or at `SILENCE`, the
  10 s every read has, from a primary gone silent; a connect is walked
  away from within 10 ms, the back-off within 20; else the thread is
  landing one message under the lock, an image's load and adoption among
  them, and its fsync. A primary sending a byte in each `SILENCE` kept a
  Windows read begun after the cut, and the halt, going for as long as it
  liked. A promotion and a tenant's release end within that, and a `get`
  of the tenant and the node's shutdown, which wait for the slot a release
  holds as it halts. `tests/primaries.rs` in fennec-server holds a primary
  gone silent in the middle of a message, the follower blocked reading it:
  its promotion answers at once. That passes without `Watched` on Unix,
  where the cut ends the read; `replication::tests::a_halt_a_read_in_a_message_missed_ends_at_the_next_byte`
  holds `Watched` itself on every platform: a halt come after the look
  between messages and its cut missed -- the flag alone, as Windows' cut is
  to a read begun after it -- a primary sending a byte every 20 ms, and the
  thread's end waited for `SILENCE`; with `Watched`'s look taken out it
  read on and the test failed at its bound (the review's second round, run
  so before the look went back).
- *A halt is looked at past the socket's reads too* (`Follower::halted`):
  as an image has arrived, and once it is loaded, before `Changing`. The
  bound above held an image's `check_records`, `load` and adoption whole --
  minutes for many gigabytes -- for a promotion, a release under the
  tenant's slot, every `get` behind it and the node's shutdown. Walked away
  from, the copy holds what it held and follows on from there once
  started again. A halt that comes while the check and load are under way
  still waits for them -- nothing looks inside `load`, minutes for many
  gigabytes -- and walks away at the second look, before the adoption.
  What a halt can still wait for besides is an adoption begun: the
  image written over the copy's file and synced, a write of the image's
  length, then the copy's memory pointed at it; or one message landing
  and its fsync. `a_halt_as_an_image_arrives_or_is_loaded_takes_none_of_it`
  has the halt come at each look (a test's seam, `halt_at`, which sets the
  flag at that look and is compiled into tests alone), the thread ending
  with the copy at change 0 and no image taken; with either look taken
  out, the follower took the image and the test failed.
- *A peer's words reach a log line and the status's `error` cut and
  escaped* (`peer_words`): the first 200 characters, as Rust writes a
  string -- a newline `\n`, an escape character `\u{1b}`, a quote `\"` --
  and the bytes there were where that is not all. An `E` message's text
  (a mebibyte, any bytes) and a refusal's body (64 KiB) went in as they
  came, while the status line was cut to 80: whoever answers at a
  primary's address, or stands on a plain-HTTP hop before it, wrote lines
  of its own into the log and a mebibyte into it and the status at every
  reconnect. One helper at every place a peer's bytes reach either -- a
  primary's `E` (`decode`), a node's (`node_archive`), a refusal's body
  (`answered`: a follower's, an archiver's, a backup's, a node archiver's)
  and a status line (`read_head`); `a_peers_words_are_cut_and_escaped`
  holds each with a mebibyte holding a newline, an escape and quotes. A
  router's log of a node's answers and the errors it answers with -- `said`,
  a promotion's refusal, every `{status}: {body}` of a node's -- went in as
  they came too, from anyone at a node's address or on a plain-HTTP hop;
  they go through it now (`peer_words_within`, fennec-shard's `words`, the
  third round), cut at 2 000 characters rather than 200: a node's refusal
  is shown whole, the lineage one a failover meets 313 bytes, whose `force`
  a cut at 200 took off (fennec-shard's
  `a_standby_that_never_synced_is_promoted_by_force_alone_and_its_read_replica_follows`
  failed so). `a_nodes_words_are_cut_and_escaped` holds `said`, `words`
  and `expect` to it.

The stream's framing is a parser of untrusted bytes, fuzzed as one: the
`stream` target ([conventions.md](conventions.md#fuzzing)) hands
`read_answer` -- the head, each message held to its bound, decoded -- a
replica's stream and a node's, whole or a few bytes a read, its seeds a
primary's answer, a node's, a refusal and the two findings above. An
image its hello promised, accepted as the follower accepts one (`loaded`,
the follower's own path: its records' sums, the record saying it follows,
the load, the change its hello said), is adopted by a copy mapped as a
server's -- its sink an in-memory one that hands the rewritten bytes back
as the mapping -- so `Database::adopt`'s `repoint` walks the image's
records and `rebind` binds its statements, which no target reached before
(the review's second round, where it was suspected and not settled), and
every collection of the copy is read after (`an_answers_image_is_adopted_by_a_mapped_copy`
holds the path to the adoption). The seeds' answer holds such an image.

### An image sent as it is read

**An image goes out as a pin takes it, and is written as it is read**
(`Database::pin_image`, `PinnedImage`, `replication::image_message`). A
replica's first follow, one past the feed's buffer, a backup (`fennec
backup`, the stream's `archive=1`), an archiver's image and a node's archive
stream's (`node_stream.rs`) and a move's image (`GET
/_admin/tenants/<t>/file`, `admin::export`) are each the database's image,
and every release from 0.1.5, replication's first, to 0.2.0 built it whole
in the primary's heap under the read lock (`snapshot()`) and wrote it as one
message: a second copy of every live document beside the database, three
for a follower that reads no sums (`plain_unless` copied it again, and
`plain_records` once more inside it). The soak harness found it (P6, round
6): 250 000 records, 259 MB, under a container of 256 MiB, the primary was
killed out of memory 0.7 s after its replica began to follow, twice of two.

Under the read lock `pin_image` runs the image's own writer
(`Database::snapshot_into`) into a `PinnedImage`, which copies every byte
but a collection's documents -- the counter, the history, the schemas,
each collection's index of its frames (a few bytes a row), its graphs, the
statements and their marks -- and takes each collection's documents as
`Store::pinned` takes a pinned read's: the id index copied, 12 bytes a row,
the mapped file and the segments shared. Its length is known then, so the
message's head says it; `PinnedImage::write_to` writes it with no lock
held, each collection's live records read from its pinned store a document
at a time (`Store::write_live`, the same call the snapshot makes), into the
connection's buffer. A record's sum is made as it is written out
(`Framing::Later`: `Summed` marks where a record begins and where its sum
goes, and the writer carries the sum over the bytes between): summed as
taken, every document would be read under the lock. A follower that reads
no sums is sent the image taken plain (`Framing::Plain`: each record's head
framed with its body's length, no claim, no sum -- what `plain_image` makes
of a snapshot), its history record written through `put_record` natively so
that it is framed as the rest are; the browser module writes it as it did.
Nothing a write changes in place is held: an append to the open segment
copies it first (8 MB at most, once an image), and a handover or a compact
lets go of its own hold only, the mapping keeping the file it renamed over;
on Windows a cut of a torn tail at an open waits for the image's view to go,
as for a pinned read's (`cut_refused` names both). A move's image is the
tenant's as it is frozen; an image a node's stream holds until the tenant's
disk covers it (`Pending`) holds the pin, never a copy.

The bytes are the snapshot's at the image's change, to the last one:
fennec-core's `pinned::a_pinned_image_is_the_snapshot_at_its_pin` holds
`pin_image` to `snapshot()` and `plain_image(snapshot())`, over a database
with every index kind, a graph with tombstones, a field added, a named and a
scheduled statement and a history, its documents in the mapped file and in
memory, written out after deletes, updates, a field added, a statement
defined, a handover and a compact beside it, a few bytes a write and whole.
The follow's numbering and durability are as they were: the image is taken
where `snapshot()` was, at the change judged under the same read lock, and
sent once the feed is durable to it. What it holds is a constant beside 16
bytes a row: `message_peak`'s
`a_primary_sends_an_image_holding_a_constant_however_large_the_database`
counts every thread of the process while an in-process primary sends a
replica its first image -- 1.29 MB at the most for 8 000 rows of a kilobyte
(an image of 8.1 MB), 1.70 for 32 000 (32.5 MB), plain alike, the
connection's 1 MiB buffer among it -- and failed on main's path (12.4 MB
for the first, past its 3 MiB). Over 250 000 rows of a kilobyte, a file of
253 MB and an image of 254 (macOS arm64, release, three rounds, a load of
1.6-1.8; the process's instructions retired less an open's): main's way
held the lock 55-56 ms, held 257 MB more than the open and took 426-430 M
instructions; the pin holds the lock 4.7 ms, holds 6.2 MB more -- 12 bytes
a row of id index, 4 of frames' index, the buffer -- and takes 347-348 M,
the whole image written in 40 ms against 55. Plain, main's held the lock
113-199 ms and 764 MB more, 724-766 M instructions; the pin's 4.7 ms, 6.2
MB, 210-211 M (no sum to make). Rejected: an image spilled into a file
beside the database under the lock and sent from there -- a heap of a
constant, but the lock held for a disk write of the image, longer than the
copy it replaces, the image's bytes written and read twice, and a file a
crash leaves to sweep; the image's stretches held as the browser module
holds them (`Kept`) -- a live image is written a document at a time, so a
piece a document, three times the id index; the frames' index made again
from the pin as the image is written, 4 bytes a row fewer held, for a second
walk of the index under the lock to say the record's length.
fennec-server's code grows 9.6 KB for it (macOS arm64, release, its text
against main's): the pin and its writer, a move's answer, the node stream's
writer; the export writes through the replication stream's own writer
type, whose code the binary holds already -- through one of its own it was
13.4. The browser module is the same size: every change to the image's
writer is native.

What takes the image takes it as it arrives (next section).

### An image taken as it arrives

**Whatever takes an image writes it to disk as it comes, every record held
to its sum as it streams, and adopts it by a rename** (`replication::take_image`,
`Database::from_side`, `Database::adopt_side`). Until this a replica read
its primary's image whole into memory (`read_kind`), checked it there
(`check_records`), loaded it into a database in memory (`loaded`) and
wrote it over its file under the write lock (`adopt`, the sink's
`rewrite`), every document of it held two and three times; `fennec backup`
read it whole, appended its fork record and, sealed, sealed a second copy;
a move's router read the source's answer whole (`Pool::call`) and sent it
as the target's request body, which the target's connection read whole
(to `--max-import`, 1 GiB) and `Tenants::import` replayed in memory before
its rename; an archiver read a hello's image whole, a node's archiver each
tenant's; and an archive's consolidation read its assembled end whole,
loaded it in memory and took a snapshot of it. So a database larger than
the memory of the machine taking it did not follow, back up, move or
consolidate, whatever the primary held.

Each takes it the one way now, a stream's bytes copied into a file as they
are read and checked as they pass (`take_image`: `fennec_core::engine::check_reader`,
`fennec verify`'s check, a stretch and a record's head held at a time;
a stream cut short is `UnexpectedEof`, a halt or a timeout the reader's
own error, damage refused naming the record):

- **A replica** (`Follower::session`, and the `stream` fuzz target's
  `read_answer` over a copy mapped as a server's) writes it into a file
  beside the copy's, `<file>.taking` (`Sink::image_side`: a name of its
  own, never `.beside`, which a replica's own `compact` may be writing
  meanwhile), made by `fs::SideFile` (`create_private`: owner-only, no
  link followed), with no lock held -- reads and the copy's upkeep go on --
  then the record saying the copy follows the hello's lineage after it, and
  syncs it (`arrived`). The copy is loaded from that file as the copy is
  held (`Database::from_side`): mapped, its documents stay where they lie
  and only what an open of the file builds is in memory, read into memory
  where the copy is (`--no-mmap`), an image cut short of a whole record
  refused; held to the hello's change (`loaded`). Under the write lock the
  sink renames it into the copy's place (`Sink::adopt`, as a `compact`
  beside the database puts its file there: held to its handle's name by
  `still_named`, given the file's mode, the sync log forgotten, the
  directory synced) and the stores the load built stand, their file the
  copy's now: nothing written under the lock but the rename, and nothing
  walked. A copy whose sink has no file -- the fuzzer's, a test's; a
  server's copy is a file's -- takes it in memory, grown as it comes.
- **`fennec backup`** writes it into the backup's side file, its fork
  record after it, sealed a frame at a time where it is sealed
  (`seal::Sealing`: a frame held until the next byte or the end says
  whether it is the last, the frames `seal::seal_whole`'s), into an archive's
  directory as an archiver's image (`Archive::add_image_with`). Its sums
  are checked now; read whole, a backup of a damaged image was written as
  it came.
- **An archiver's image**, a hello's (`Archive::hello_with`) and each of a
  node's tenants' (`node_archive`, which passes over what an image refused
  part way left unread before the next message, as it passes over a
  tenant's it does not archive), is written into its file as it comes,
  sealed as it goes (`Archive::put_with`); an image of another history
  (`Took::Diverged`) is not read at all.
- **An archive's end** (`Archive::end_image`, `consolidate`, and the image
  a line sealed before version 3 goes on from) is opened mapped through the
  handle its round's file was assembled with (`fs::mapped`) and its image
  written from a pin as it is read (`PinnedImage::write_to`).
- **A move** relays the image through the router as it comes: the
  source's answer read as a body (`Answer::body_reader`) into the target's
  install on a fresh connection (`Pool::call_streamed`, which a body read
  as it goes cannot be sent again on), asked of the frozen source again
  where the target answered 409. The target reads an install that says its
  length as it comes, before any other route (`admin::import`, past
  `open_refusal` alone of what is above it): into `<file>.importing` beside
  the tenant's, checked as it streams, synced, opened from there mapped to
  check it whole as the node opens its tenants, mapped or not
  (`Tenants::import_from`), then renamed into place under
  the tenant's slot and lock as before (`still_named` first). One refused
  before its body is read whole ends its connection with the answer; a
  chunked install is still read whole, to `--max-import`.

What a crash leaves: before the rename, the copy's file as it was and the
image beside it, whole or not, which the next open never reads and the
follower, the one writer of that name, removes as it starts; after the
rename, the image with the record saying the copy follows, which the next
start opens as a copy of the primary's lineage at the image's change -- the
image taken inside its rename, as before. A torn image, a refused sum, an
image at another change than its hello's, a load that fails, a halt before
the adoption: the side file goes as it drops, the copy and its file as
they were, to the byte (`tests/images_taken.rs` builds each state). The
follow's order, numbering and durability are as they were: the image is
adopted under the same write lock, its history followed and the copy's own
feed started on it there, and the times a copy keeps for its writes begin
again with the image, as they did.

What each holds is a constant beside what its own open of the image
builds: `tests/message_peak.rs` counts every thread of the process --
primary and taker -- while 8 000 and 32 000 rows of a kilobyte (images of
8.1 and 32.5 MB) are taken: a replica 4.4 and 4.9 MB, `fennec backup` 4.5
and 4.9 into a file, 7.6 and 8.1 sealed, 4.4 and 4.9 into a directory, an
archive's end 1.4 and 2.5 MB; fennec-shard's `tests/move_peak.rs`, the
router and both nodes in one process, 3.4 and 3.9 MB for the move. The
buffers are most of it: the stream's (1 MiB), the check's (1 MiB and 64
KiB), the side file's writer, the primary's own (1 MiB), a sealed frame and
its seal (2 MiB). On the receivers before this each failed at the first
size: the replica held 19.5 MB, the backup 26.5, the archive's end 32.5,
the move 24.5. FIGURES_FOLLOW

Still read whole: a sealed archive's base image as a restore or a
consolidating round assembles its end (`read_whole`, then `open_whole`),
and a segment, 64 MiB at the most, as either reads it; a sealed backup
as `fennec unseal` opens it (`seal::open_whole`); a move across
regions, which the global router relays through the regions' routers as
requests and answers read whole; and an install sent chunked. A node takes
an image of 1 GiB at the most (`max_import`, which no flag sets): a body's
ceiling is judged from its head (the conn rule), whether or not it is read
whole.

### A copy across a compact

**A copy agrees with its primary across a compact because a compact
changes no place** ([storage.md](storage.md#dropped-places)). A compact is
no record: it rewrites the file, marks the feed durable and sends nothing,
so a copy, an archive or a standby holds whatever places it held, and
applies every record after the compact by them. A compact that took a
dropped field's places out -- every release from 0.1.7 to 0.2.0 -- left
each copy reading the collection's later writes one field early. A copy
that compacted itself first -- a server's own compaction runs on a
following database from 0.1.10 -- read them one field late. An archive's restore
from an image before the compact misread them, and so did its images
after. A `--dir` node's standby misread them, and a failover made those
values the tenant's. Kept places, each one's value made a `null`, leave
every copy's documents readable by the primary's places whichever of the
two compacts, and in whatever order.
`tests/replication.rs` holds a copy caught up before its primary's compact,
one behind that catches up across it by the records, one connecting after
it, and one compacting first, each over real replication. `tests/archive.rs`
holds a restore from the image before the compact, its `verify`, and an
image of the archive's own end. fennec-shard's `tests/replicas.rs` holds a
standby and its failover, the promoted copy's file read on its own. The sync
protocol's replicas take rows as values and write them by their own schema
(`fennec_abi::sync`), so no place reaches them; `fennec-abi`'s
`tests/sync.rs` holds one across a drop and a compact on its server.

*A 0.2.0 primary.* Replicas upgrade before primaries, so for as long as a
0.2.0 primary runs, its compact still takes the places out and tells
nobody. A newer copy tells at the primary's next write of that collection:
it reads each write as it was made (`Schema::read_written`), a value at
every place up to the last field and none past it, as every release since
0.1.7 encodes one (`Store::encode_doc` writes a `null` at every dropped
place a field follows), and a write made by other places than the copy
holds -- one value short where the primary took a place out, one over
where the copy did, under a release that compacted it -- is refused by name
before anything reads it: "the primary's writes moved the fields of `x`:
... seed the copy again from its primary". An alter that comes first is
refused alike: it carries the schema after it (record kind 12), whose
dropped places the copy holds to its own, the same or one more for a drop
(`keeps_places`); a `create index` carries the whole schema, held to the
copy's layout as it was. Where no field followed the dropped one, its
place was last in every document, which a write leaves out: the primary's
writes after its compact hold what the copy's would, read alike on both,
and the copy tells at the primary's next alter or `create index` of the
collection instead. A server's follower takes an image of its primary
on a write it cannot apply, and holds what the primary holds from there,
so no value read a field off reaches a copy's file. The check is the end
of the read `apply` makes of every write already, and costs the copy a
comparison a write; a check at the compact itself would need the compact
to say so in the feed, a record kind, which a format change and an older
copy's refusal would bring, for a window 1.0 closes.

*A 0.2.0 copy.* The other way round -- a 1.x primary, a 0.2.0 copy, as a
`--dir` node upgraded first leaves its tenants' standbys on the node not
yet upgraded -- a field dropped on the primary keeps its place, and the
copy's own compact takes it out of the copy's documents: it reads every
later write of the collection a field late, with nothing to check it, and
a failover makes those values the tenant's. The primary cannot tell the
copy's release to refuse the drop: a copy's request names only where it
stands (`since`, `history`, `sums`, `image`, `archive`, `reader`) with its
token, and the hello is the primary's; both are the same in 0.2.0 and 1.x.
The compatibility contract's rule is to drop no field until every node and
every copy runs 1.x, or to run each 0.2.0 copy with `--auto-compact off`
until it does.

An archive does not tell: it keeps the stream's bytes, and a restore is an
image and the writes after it opened (`Archive::restore`, `fs::open`),
which reads a payload by the schema it holds and never refuses one for its
length -- an open reads the documents of an image written before an `add
field`, one value short, as every image has. A restore from an image
taken before a 0.2.0 primary's stripping compact, to a moment after it,
reads the writes after it a field early, as every image the archiver
takes from there does. What an operator does is the compatibility
contract's ([compatibility.md](compatibility.md#a-020-node-and-a-compact)).
`tests/replication.rs`'
`a_copy_of_a_primary_that_took_the_places_out_is_seeded_again_at_its_next_write`
holds a follower of a stripped primary seeded again at the write, and
fennec-core's `compact_places::a_copy_refuses_a_write_made_by_other_places_than_it_holds`
both directions.

## A write's time

**A copy keeps the time its primary appended each write at, and an
archive's times run in order.** A primary's feed stamps each record with
its clock as the `Tee` hands it over (`Feed::push`), and its stream sends
the times beside the records (`W`: the first write, a count, a time for
each record, the records). The follower took the times off the stream and
dropped them (`Message::Writes { first, records, .. }`), and applied the
records, which its own sink's `Tee` stamped with the copy's clock as each
landed: a copy's feed timed every write at the moment the copy applied it,
a replication lag after its primary appended it, and so did everything
read from that feed -- a copy of the copy, the copy's `/_changes` (`at`)
and an archive cut from it: a `--dir` node's `archive --tenants` covers
the tenants the node holds as a copy, and `fennec archive` may be pointed
at a replica. A restore to a moment from such an archive left out writes
the primary had appended, and answered, before that moment, where the docs
promise every one. It was found root-causing a failed failover drill, and
was there from the first release with replication: v0.1.5 to v0.2.0.

The follower now hands the times to its feed after `Database::apply`
(`Feed::sent_times`), under the write lock and before its flush, which is
what makes a record it applied durable, so sendable: no record is sent
before its time is its primary's (a debug assertion holds it). The records
the apply landed are the ring's newest, one entry a record -- a block that
spilled and landed (`land`) is sent as the one record it would have been
(`landed_block`) and applied as one. Where the apply says how many landed
(its `Ok(n)`: every record of the message) they are paired from the last
back, which leaves them right where a ring smaller than one message let go
of the first of them as later ones came in (only `--replication-buffer 0`
or `1` can be). An apply refused part way lands a prefix, the records after the
refused one put back and never handed to the sink, and says nothing: its
records are paired from the first on, where the ring still holds the first,
and otherwise -- under `--replication-buffer 0` or `1` alone, the one
place the rule's "nothing is sent with the copy's own clock" does not
reach -- keep the copy's time until the image the follower connects
again for starts the feed over (`Feed::start`), as any image (`force_image`)
does; the writes after an image take their times as any. A copy of a copy
sends its feed's times, its primary's, so a chain carries the first
primary's times to its end; a promoted copy has no follower, and its own
writes take its own clock from the promotion on.

Rejected: handing the times to the feed before the apply, `push` taking
the next one -- right from the first moment, no window to reason about --
since `push` is a primary's write path, and would look at a list there on
every write. Rejected too: counting the message's records' writes to match
them to the ring by number (`writes_in` a record, as `push` counts them),
which also matches a refused apply's prefix whatever the ring let go of: a
third caller of `writes_in` had the release build's LTO stop inlining it
into `Feed::push`, read off the binary -- 358 instructions and a call
where `main` inlines it in 519 -- a call more on every write a primary
makes. As built, `Feed::push` and each of the `Tee`'s sink methods are the
same instructions as on `main`, addresses aside, every `Feed` and `Tee`
function but the new one (`fennec-server` built with `-C strip=none`,
`objdump` of each symbol, macOS arm64). Nothing in the source says LTO
inlines `writes_in` into `push`, so a change near it -- another caller of
`writes_in` above all -- owes the same look: `objdump -d --demangle` of
`fennec-server` built `--release` with `RUSTFLAGS="-C strip=none"`, no
call to `writes_in` inside `Feed::push`.

*An archive's times run in order.* With a copy's writes carrying its
primary's times, an archive that follows a copy through its promotion is
timed by two clocks, the old primary's to the fork and the copy's after it,
and a copy whose clock is behind sends writes timed before the ones before
them, as a clock set back does on one machine. A restore to a moment stops
at the first write timed after it, a prefix either way, but `prune` and a
restore to a moment before every write kept read the images' times -- a
consolidated image is timed by its last write (`time_of`) -- as in order:
`prune` took the newest image timed by its cutoff for the base and let go
of the images before it, one of which a restore to a moment inside `--keep`
started from, and that restore answered change 8 after the prune against 3
before it (`archive::tests`' `an_archives_times_run_in_order_across_a_clock_behind`).
Now `Archive::take` archives no write at a time before the latest it
archived (`Writer::latest`, read back from the last segment as the archive
is opened to be written, so an archiver started again goes on from it): a
restore to a moment cuts where it cut, the first write past the moment
being the same, and every time an archive holds runs in order. Rejected:
keeping the feed's times in order instead (the latest in `push`), on a
primary's write path, and lost with the process: a copy promoted after a
restart had no time of its primary's to hold to, where an archive keeps its
latest on disk. The image a hello brings is timed by the archiver's clock
as it takes it, and no earlier than the latest write the archive holds
(`Archive::hello`, `Writer::latest`): by that clock alone, behind the
writes', it was dated before writes it follows, and a `prune` whose cutoff
fell between the two took it for the base and let go of the image a
restore inside `--keep` started from, which then found none to start from
(`a_hellos_image_is_not_dated_before_the_writes_the_archive_holds`).
A stored time is so the time the write was appended or, where the clock
stepped back, the latest one stored before it, which `verify` and a
restore report. The cost: a clock that ran ahead by some time holds
pruning back that long, since every later write and image is stored at
that time until real time passes it, and `prune` keeps the images timed
after its cutoff. A copy's `/_changes`
hands over each write's `at` as its feed holds it -- its primary's, then,
promoted, its own, which may be behind -- a reading of a clock, as on a
primary; a consumer orders by `seq`.

`tests/replication.rs`' `a_replica_and_an_archive_of_it_keep_the_times_its_primary_appended_at`
holds a replica back while its primary writes -- a statement's writes and a
block that spilled and landed -- until the clock is past every time the
primary appended them at, then lets it apply them: the replica's
`/_changes`, a replica's of it and a restore to the moment the primary
appended the last of them, from an archive cut from the replica, each
hold the primary's times, again after an image the primary sends it
(buffer 64 KiB), and promoted the replica times its own write by its
clock. On `main` it fails at its first comparison, every time the
replica's. `replication::tests`' `a_followers_feed_holds_the_times_its_primary_sent`
and `a_followers_records_the_ring_kept_take_their_own_times` hold a
whole message, a refused one's prefix and a ring that let go of a
message's first records. The cost: `fennec-server`'s `__text` 1 024 bytes
more (4 819 520 to 4 820 544, 16 bytes on the file; macOS arm64, release);
a copy's apply walks the records it landed in its ring once more, under the
lock it holds anyway, and an archiver compares a time a write.

## A node's archive

**A node's tenants are archived over one stream** (`fennec archive
--tenants`, `fennec_http::node_stream` on the node, `node_archive` the
archiver). A tenant's own stream is a connection and a thread on the node
for as long as it runs, one of `--max-connections`, and holds its tenant
open: archived so, a node of 1 000 tenants of ten rows and one of 64 MB held
1 016 threads, 4 011 descriptors and every tenant open for good, against 15,
27 and the 10 written with no archiver -- and at the default 100 connections
could not be archived at all. `POST /_replication/tenants`, with the
replication token, is one connection and one node thread for every tenant:
its body is where each archive under the root stands (`<tenant> <since>
<history> <image>` a line), the node says hello to each tenant it holds (an
image where the place does not lead to its writes) and `G` to each listed
one it does not, and every message is the tenant stream's led by its name.
It sends a tenant's writes from its feed while it is open, and nothing while
it is closed, whose file does not change. It is a sender on each open
tenant's feed as a replica's stream is -- put there as the tenant opens
where its client stands where the file does (`NodeStreams::attach`), and on
the tenants open as it connects, before it reads which tenants there are
(`begin`: read first, a tenant made between was in neither) -- so a `--sync
always` write waits for it, and `Tenants::close` refuses while a sender has
a write left (`Feed::unsent`), syncing a dirty tenant first: closed, its
feed went with it and the writes came again as an image. One stream for
every tenant holds no tenant up for another: its sender carries a flag
(`Sender::held`, `Listener::hold`) set while the stream does what can block
-- a closed tenant read or opened to be looked at, a tenant's lock waited
for (`Listener::read`: waited for unheld, a tenant write-locked as the
stream came to judge it held another's next durable write 1.014 s), an
image's snapshot and its sending, a socket the archiver reads slower than it
fills (written non-blocking, `Out`, the flag set before a write that would
block) -- and a durable write waits for no sender held so
(`Sender::waited_for`); a stream that starts behind, at an image or on the
tenants open as it connects, is lagging until it catches up, and an image
waits for its tenant's disk among the stream's other work (`Pending`, looked
at again every 50 ms) rather than in `DURABLE_WAIT`'s 30 s. Its feed is
among the stream's while it waits, where a close takes it off
(`NodeStreams::detach`) and an open puts another in its place, `attach`
dropping a feed of an earlier opening, and `send_pending` sends only while
it is still there, taking the tenant again otherwise: an image pending
across a close and an open put the dead feed back once sent, the stream read
nothing from it again and the archive stood still -- the node at 8, it at 5
-- until the archiver connected again. Held up the replica's way, every
tenant's next durable write waited out `SENT_WAIT`, 1.008 s, while the
stream opened another tenant. The syncer of `--sync <ms>` and a close sync
waiting for no stream (`replication::unwaited`), since no client's answer
waits on them: waiting, a pass took 5.06 s over five tenants beside a stream
busy elsewhere, the writes of the pass unsynced meanwhile, and a close held
its slot's lock 1.01 s. So an archive of a node is synchronous only while
the stream keeps up, and under load it is not: a write answered while the
stream is held up or behind is on the node's disk and not yet in the
archive, sent once the stream comes back to its tenant -- at worst inside an
image -- and missing from it if that disk is lost first; a replica's own
stream keeps a write's answer waiting up to a second instead.
`tests/node_archive.rs` holds a tenant's open, a tenant's lock, an image's
disk and a full socket each beside another tenant's writes, and fails with
the flag, the held lock wait, a blocked write's hold or the pending image
taken out (a write beside an archiver that stopped reading waited 1.014 s)
-- the unwaited sync is `replication`'s unit test's to hold
(`the_syncers_durability_waits_for_no_stream`), since beside a stream held
up a sync waits for none either way; it asks `Feed::waited_out`, the durable
writes that waited out a stream, rather than a clock, which a runner slowed
by its neighbours fails. Which tenants have news is told: a feed tells as
its writes reach the disk, it starts over or closes (`Feed::tell_with`, held
strongly: a `Weak`'s upgrade was a compare-and-swap on one count every
tenant's sync shared, 139-141 ns a tell from 16 threads against 0.20-0.24),
and a tenant made, installed, deleted, promoted or made to follow touches
the stream; one released while the stream is busy with it (`Next::Gone`) is
looked at again until it is gone or back -- a tenant made again under the
name, writing nothing, gave no notice more -- and the directory is read
again every 10 s for a file put there by hand. With no stream connected a
sync pays two loads, 13 ns a durable advance either way. A closed tenant's
place is where it closed in this process (`Tenants::closed`) or read from
its file (`Tenants::peek`, `open_read_only`), where opening to look took 6.3
ms a tenant, the open fsyncing the file; a closed tenant the stream is told
of is read again, since a holder of its database may have written it after
the close, and one with a non-empty sync log beside it (Linux, after the
machine went down) is opened, the log holding durable writes the file lacks.
The feed keeps the lineage the database last wrote, read off the history
records `Tee::append` passes, and the stream sends it (`L`) before the
writes after a promotion: a promoted standby's archive goes on with no
image. The archiver keeps `<root>/<tenant>/` as `fennec archive` keeps one,
through the same `archive::Writer` the single archiver now writes with; a
segment file is open only while its tenant writes (a thousand would pass a
macOS shell's 256 descriptors), the `history` file is written only when it
moved (a thousand fsyncs a reconnect otherwise), and `archiving.lock` keeps
a second archiver off the root. A tenant the node says is gone -- deleted,
or moved to a node whose archiver takes it up from its image on the same
history -- keeps its archive, and `presence` notes `gone <ms>`, then `back
<ms>` if it returns: a restore to a moment it was away leaves it out, as one
deleted, where forgetting the time away gave the copy from before it left.
The times are the node's clock, as its own writes' are (a tenant it holds
as a copy has its primary's, [above](#a-writes-time)): `G` carries when this
process deleted the tenant (`Tenants::departed`, the last 65 536, kept only
on a node that replicates its tenants and noted once the file is gone: noted
before it, a removal refused had the archive read the tenant gone and back
62 ms later) or else when the stream saw it go, and `back` is the node's
hello -- stamped as the archiver heard, a moment after a delete still
restored the tenant while the stream was busy or the archiver away. A tenant
deleted and made again while the stream was busy is told gone at the delete
before the new copy's hello (`Session::left`), and a line moved aside for a
copy heard of alone, the archiver away meanwhile, is noted gone by that
hello at the latest; `away_at` reads the last line noted by a moment, a
clock set back leaving the times out of order. An image at or before the
archive's end on a history that does not lead from it
(`archive::Took::Diverged`: restored to an earlier moment, or another tenant
of the name) would have landed among changes of the same numbers --
`File::create` of a segment of the same name wrote one over -- so the single
archiver refuses it and the node's moves the line aside to
`<tenant>.<history>` and begins a new one. `restore --tenants` to a time
takes, of a tenant's lines, the one that began last by then
(`node_archive::lines`; `Archive::began`, its oldest image's time, which a
`prune` letting that image go writes into `began` first -- written with the
first image it was an fsync more a tenant of the stream's first pass; sealed,
each image's head says it and `began` is held to that, and `presence` is
sealed, [below](#sealed-archives)), and
refuses a moment `--keep` let go of (`Restoring::LetGo`, exit 1) rather than
pass the tenant over as never archived; `verify` and `prune` run on every
line, and `Archive::verify` names a gap an image alone ends. One tenant's
archive that does not read is left for an operator and the rest go on, and
so is a tenant whose stream sends a record that fails its checksum or carries
none after ones that do (`Archive::take`, refused as damage): passed over,
said, until the archiver follows the node again
(`a_record_that_fails_its_checksum_passes_over_its_tenant_alone`). Until
the fourth review of #568 that refusal ended the node's stream, and the next
sent the same record: no tenant of the node was archived. The single archiver
makes its stream again, which a record whole in its primary's file then
passes.
Measured with `make node-archive-bench` (1 000 tenants of ten rows and one
of 64 MB, closed when idle, ten written ten times a second from before the
archiver connected and for 20 s after, under `--sync always`, in turns): the
node held 16 threads, 29 descriptors and the 10 written open, 3.9-6.0% of a
core against 4.7-5.1 with no archiver and 7.0-8.0 a stream each; the
archiver a thread and 21 descriptors against 1 001 threads and 2 021; the
writes while the first images were taken, the 64 MB one among them,
14.5-14.7 ms p50, 32-35 p99 and 43-116 at most, where a stream each held
them 176-600 p99 and up to 1.1 s, main's build alike (86-351, up to 994 ms);
after them 12.4-13.8 p50 against 11.8-12.1 with none (`F_FULLFSYNC`); a new
archive's first images, a tenant at a time, 14.8-15.2 s against 11.2-15.5
for a thousand streams; after a restart 2.2-4.6 MB and no tenant open
against 121-157 MB and every one, all 1 001 said hello to in 0.29-0.33 s.
Under `--sync 250` (`node_archive_passes`: eight tenants written every 5 ms,
eight opened, written and closed in turn, one of 32 MB closed) the syncer's
pass took 52 ms p50 beside the node's stream, 91-168 at most -- the
archiver's fsyncs on the same disk -- against 31-33 with none and 65-86 a
stream each, and a close 4.0 ms p50, 15-33 at most, against 3.8-4.0 and
12-25 with none. With no archiver and with a stream each, every figure
stayed main's in ABBA turns (the pass 31.0-31.4 and 67-73 ms p50 on main, a
close 3.6-3.9) -- the pass every dirty tenant synced in turn, as a node
synced them before [each had a thread of its
own](durability.md#a-nodes-syncs), which the example still times. The pending feed, the lock held up and the departures moved
nothing, four turns a side against the build before them: the writes while
the first images were taken 13.9-15.3 ms p50 and 26.7-32.9 p99 against
13.9-14.9 and 29.5-30.6, the first images 14.8-16.8 s against 15.1-16.8, and
with no archiver 11.5-12.1 ms p50 against main's 12.0-12.1. `make
replica-bench`, four turns a side at a load of 10-15 from other work:
`--sync always` lag 0.10-0.15 ms p50 against 0.10-0.11, catch-up 11 128-13
614 writes/s against 11 384-13 857, a restore to the end 7.87-9.09 s against
8.51-9.05, each pair inside its turns' spread -- a replicated write meets a
byte compared in `Tee::append`, a thread-local in its flush and a flag in
`wait_sent`. A node's request path (`make shard-bench ARGS=latency`, a
tenant's read straight to the node) stayed 21.5 us p50 against main's 21.6,
eight turns a side, with the stream's handler out of line and cold (inlined
into the connection loop, four turns a side read 21.5-22.0 against
15.7-21.5: main ran in a 15-16 us mode three times in twelve, this build
never). Nothing below `fennec-http` changed.

An archive's names are made durable before what stands on them, not each
as it is made (`Archive::sync_dir`, through fennec-core's `fs::sync_dir`):
until its directory is synced a crash can lose a file made or renamed in,
or bring one removed back, in any order on some file systems. A new segment
syncs the directory after its magic and before its first write, so its name
lasts as long as its writes do -- an fsync of the file alone left a segment
written for an hour to a crash -- and the names before it are on disk
first: a segment landed without the image a hello brought began after
changes no segment holds and no image ends, which no restore passes, and a
crash before the sync leaves a segment holding no write, which `position`
removes. `prune` syncs before it lets anything go (the image a restore now
starts from, renamed in by a hello or `consolidate` with no sync of its own,
and `began`) and again between the images and the segments, since an image
brought back with the segments after it gone is one `verify` refuses; a
segment brought back holds writes before the oldest image, which a restore
passes over. A history moved with no image is synced at once, the writes
after it going on in the open segment, and an image taken where the
archive held one is synced before the history naming its lineage: landed
alone, that history had the primary send a fork's writes on after the old
lineage's. A first image is not, since an fsync more a tenant made a node's
first pass 44% longer (`keep_began`): a hello's image whose history a crash
lost is read for it where the history file would refuse the primary
(`ends_on_image_of`; `an_image_whose_history_a_crash_lost_goes_on` leaves
that state by hand and was refused as another database without it), and a
history without its first image asks for one again. That image is held to
its change first, as `verify` holds every image
(`an_image_of_another_change_than_its_name_is_refused`), and a sibling fork
is taken too: a `lineage` move with nothing written after the image leaves
no write of that fork in the archive to mix with the primary's, the state
a crash before that move's sync leaves anyway, and the fork saying hello
later is refused as any is. A restore, a backup and an unsealed file are
synced into place before they return, and a node's root once as it is
made; a tenant's directory is synced into the root by the first segment
written after it was made, once for every tenant made before that sync
(`archive::Parent`), where each tenant's first segment paid two fsyncs on
the one stream thread, the root's beside its own. A round's
`consolidating-<end>.fennec`, the side files and a line moved aside are
not synced, each harmless
brought back or lost, or moved again by the next hello. A directory sync
that fails is known to be retried, as a segment's failed fsync already is:
it fails the step it stands before, and `follow` (`Archive::follow`,
`NodeArchive::follow`, a tenant's hello as the node's stream) connects
again and syncs again from where `position` finds the archive, never going
on past it. The database file stops writes on a failed fsync instead
(`Database::fail`), since a file system may drop what a failed fsync left
and report a retry durable; an archive goes on, and a name a crash then
loses is for `verify` to find. A 1 GiB flat-out `take` of 1 KB records, 17
segments, four turns a side: the sync took 4.75-5.23 ms a segment
(`F_FULLFSYNC`) against
260-290 ms writing one, and the runs 4.40-4.89 s against 4.41-4.82 without
it; a feed pays it each 64 MB or each image taken (`--image-every`, an hour
unless given). On a node (`make node-archive-bench`, its defaults, three
turns a side against main, macOS): every tenant's first image took
14.1-14.8 s against 15.3-15.8, the writes as they were taken 13.9-14.6 ms
at p50 against 14.6-15.3, a reconnect 0.15-0.48 s against 0.13-0.35, the
syncer's passes 46.4-52.6 ms at p50 against 47.4-49.4 and a close 4.0 ms
both, each within the other's spread. With the root synced a tenant, before
`Parent`, the passes were 50.6-53.0 ms against 46.4-49.2 and a reconnect
0.21-0.35 s against 0.12-0.13; traced, the archiver synced nothing in a
reconnect, and in the passes only at each tenant's first segment, sixteen
in their first 0.2 s, so both were noise or that start alone.

## Change data capture

**`/_changes` reads the writes on disk, documents and all** (`cdc.rs`,
`fennec-server --cdc`; a primary's feed with `--replication-token`). A
subscription keeps a query's rows and is reseeded past its ring of ids,
which holds no documents; change data capture has to see every write once.
So it reads the records the feed keeps for replicas (`Feed::changes_after`:
from the record holding the write after the cursor, where a replica is sent
an image) and has the engine read them out a write at a time
(`Database::changes_in`, native only): numbered as the counter numbered
them, each document by its collection's schema as the database knows it or
as a create among the records made it. `since` is the last write a
consumer has and `Fennec-Next` the last one an answer holds; a cursor inside
a block's record goes on after the write it names. `Fennec-Seq` on every
answer is the last write the database holds (`Feed::seq`, read after the
page, so never short of it), on disk or not and of the collections the
stream leaves out: a `Fennec-Next` that has reached it has every write
there is, where an empty page alone could not tell a consumer that had
caught up from one whose writes were not on disk yet, and Kestrel's
worker waited out 600 ms of empty pages to be sure. Only what an fsync
covered is handed over, a cursor the feed no longer reaches is answered 410
with the first `since` it does -- never with writes missing -- one past the
last write 409, and `wait` waits on the feed's `Condvar` for a write. A
scoped token is refused: its filter could not hold back the deletion of a
row it never saw. A consumer with no state of its own has the server keep
where it is (`/_changes/consumers/<name>`, rows of `_consumers`, made by a
`POST` at the last write on disk or the `since` it names -- read from
"now" each time, it missed what came between two reads -- and moved by a
`PATCH` of `since` alone), each write at least once. A `POST` of a
consumer that is there writes nothing and answers 200 with where it
stands, for no `since` or one at or behind it: that is where a new one
starts, as Kafka reads `auto.offset.reset` only for a group with no
committed offset. A `since` ahead of it is 409 naming its place
(`cdc::ahead`): it is a commit sent as a `POST`, as every commit was
before, and answered 200 an older client -- the Python SDK's, the recipes
of the time -- read the same page for ever, told nothing (a review ran
50 identical pages). The `POST` was the commit too, and one with no body moved a
consumer that was there to the last write on disk: a relay or a follower
making its consumer at each start, the natural way to write one, skipped
every write made while it was stopped -- found writing the recipes, whose
pages made a consumer only where `GET /_changes/consumers` listed none.
Answering a consumer that is there 409, as Redis's `XGROUP CREATE`
answers `BUSYGROUP`, was rejected: every caller takes that error for a
success, and one making its consumer from the first write (`{"since":
0}`) would fail every start but the first; a caller that must know it
made the consumer reads 201 from 200. A `PATCH` of a consumer that is not
there is 404, never a make, so a commit racing a `DELETE` makes none
again. Both run under the write lock (`cdc::changed`), which refuses each
on a following database -- its consumers are its primary's, and a `POST`
of one that is there, writing nothing, had passed there, leaving a relay
pointed at a replica to fail at its first commit -- and wait for the file's
durability before the answer even where they wrote nothing, so a consumer
another request made a moment before is not answered as there before its
fsync. Each answers where the consumer stands after it -- a commit past
nothing but cursors writes nothing and leaves it where it was, which
reads the same writes, and answered as sent it named a place no list and
no later `POST` found -- and `Fennec-Seq`, so a read sent after it
(`Fennec-After`) to a replica finds the consumer as it left it. A `since`
is taken up to the last write the database holds (`change_seq` under the
lock), as a read's cursor is (`Tail::Ahead`), not the last on disk: the
feed drops chunks by size whether or not they are on disk, so a 410
names `oldest - 1`, which may be ahead of the disk, and a commit held to
the disk refused the one place the consumer was told to go on from. Taken,
the commit's row lands after it, a read from it waits for the disk, and a
crash hands the writes after it over again, never skips them. On a tenant
node a consumer's write is the tenant's (`consumer_write`): held against a
move's freeze (`Tenant::enter`) and told to come back (503) while the
tenant is frozen or its lease is out, then asked of the lease as it is
answered. Run past the gate, one sent during a move landed on the source
after its export and was lost at the flip, and a make lost so was made
again later at the last write on disk, past every write in between. The
stream leaves `_consumers`' own writes out, and every collection of the
server's own with them -- each whose name starts with `_` (`declared::own`):
`_idempotency`'s keys, `_migrations`' records, `--follow`'s `_follow`, a
hook's keys -- the
cursor going past them, and a `wait` waits on past them rather than answer
nothing at once, while a commit to where nothing but those were written
since the consumer's place writes nothing (`unmoved`, the change ring's
`changed_collections_since`): a sink that committed every answer
committed its own commit's empty answer, 30 000 fsynced writes in a few
idle minutes. A feed kept for it alone has no token (`Replication`'s
token is an `Option`: an empty one matched an empty `Bearer`). Keeping it
cost nothing measurable, 16 400 single puts a second over HTTP either way,
and 9 000 rows of 128 dimensions read back at 283 000 a second.

## Fennec-After

**A read can wait for a write on a replica** (`after` in `lib.rs`,
`Hub::reached`). A write's answer carries `Fennec-Seq`, the change it left
the database at -- a field of `Response`, written into the head with no
allocation -- and a request sent with `Fennec-After: <n>` is served once
the database holds change `n`, or answered 504 with where it stands after
`Fennec-Wait` (5 s, 30 at most): never from before the write. A replica's
applied records reach the hub through `Database::apply`'s watcher, so the
wait is the subscriptions' `Condvar`, told of the records landed however
`apply` ends -- a panic after one landed too (`Waking`,
[engine.md](engine.md#blocks)). Held back, a replica answered a read
without the header from before the write and with it after 300 ms, as the
test holds it. It costs the server nothing measurable -- 37 500 puts a
second against 37 700 from a client that does not parse headers -- while
Python's `http.client`, parsing one more, went 18 200 -> 17 900. Only a
request `handle` would take past its token waits (`admitted`: the MCP
endpoint's own checks where it goes there, `authenticate` elsewhere, and
before either `route_tenant`'s token and tenant); a refused one goes on to
`handle` at once, refused and counted by `audit` as any. On a single
database the wait ran first: a guessed token held a thread for up to 30 s,
enough of them every one `--max-connections` allows, and its 504 said
where the database stood. Every other wait already came after its token
-- a held claim is planned only past `authenticate`, `/_changes` and a
subscription ask first, a replication stream its own token, `/_admin/` the
admin token -- and the router forwards what the node refuses at once
(`tests/token_first.rs`, fennec-shard's
`a_refused_token_waits_for_nothing_through_the_router`: each refusal asks
30 s for a write made only once it is in, so one that waited would be the
wait's 504, never 401). A request without the header asks nothing more:
`make requests-bench` in ABBA turns against main kept every p50 and its
rates within 0.5%. A subscription and `/_changes` wait the same way: the
stream in `sse::serve` once its token, its tenant's stream slot and the
database's `--http-max-streams` slot are taken, and before the shape is
read -- a collection the client made may not be on the copy yet -- and
before the head, so its 504 is a read's and a router's replica path sends
it home; `/_changes` once its token, a scoped token's 403 and a feed are
asked, holding a stream slot as its `wait` does. Both came before the
wait: on a single server `GET /t/changes` sent `Fennec-After: 999999` and
`Fennec-Wait: 300` was seeded at once where `/query` was 504 after 0.31 s,
and through a router a stream marked by Go's `Read()` or .NET's
`AsRead()` -- a router sends a marked request near whatever its route --
seeded from a read replica behind the client's own write, or left on the
branch a failover abandoned, and a page's `Fennec-Seq` behind the write
read as every write there is. A stream resumed with `since` waits too:
`since` is the stream's cursor and the header the client's position, and
`since` alone asks no wait -- a primary that lost writes in a failover
never reaches a cursor of the old branch in its history, and reseeds as
before. The server writes no `id:` line, so no client sends
`Last-Event-ID`. A page waits for the change to be in the database, not
on its disk: its `Fennec-Seq` is never behind the write, and the write
comes on a later page once an fsync covers it (`/_changes`' own `wait`
waits for writes after `since`). A marked stream on a copy that has not
caught up, or refused another lineage's primary, is 503 before any of
it, as a read is, and one open as the copy deletes itself ends with the
hub (`release`). Go's `Subscribe` and .NET's `SubscribeAsync` take a
stream's 504 near as a read's (`toldBy`, `SentHome`): home's position
taken and the stream opened home unmarked -- an error, every stream near
after a write lost in a failover was refused for good. `Hub::after_waiting`
counts the requests in the wait, what the tests wait on before the write
comes: `tests/replication.rs` holds a stream, a stream resumed and a page
on a replica whose follower is halted, each seen waiting, then served
with the write, and 504 past the wait; `tests/token_first.rs` each refused
at once with the header, and a good token's stream answered by the wait
(its `ask` reads to the end its head names, so a stream served instead
fails rather than hangs on its keep-alives); fennec-shard's
`a_marked_stream_after_a_write_waits_on_the_replica_or_goes_home` a
stream and a page going home past the replica's wait, and a stream
answered by the replica once it catches up, home cut off; Go's
`TestAStreamAfterAWriteWaitsForItOnAReplica` and .NET's
`AStreamAfterAWriteWaitsForItOnAReplica` a real replica's stream after a
write it cannot hold -- its link to the primary through a proxy the test
cuts -- answered 504, and seeded with it once healed -- each failing with
its wait taken out. A stream's head names where the database stood
as it opened, as an answer does: `Fennec-Seq`, the change its seed is at
or after, and `Fennec-History` where the database has a history of its
own -- read under the lock the shape is parsed under, no lock of its own
-- so a client re-running its reads near while the stream stays home
sends them after an event's `seq` of that history, which a number alone
does not name across a failover (`tests/replication.rs`, a primary's and
its replica's head; `tests/changes.rs`, the root's naming none). The head
carries every answer's CORS headers (`cors::push_into`): it wrote the
origin alone, and a page on another origin could read no header of it.
Without the header a stream and a page look for
it once, as a read does: `make requests-bench` in three ABBA quartets
against main, a row by id 0.021-0.022 ms p50 against 0.022, a put
0.021-0.023 against 0.022-0.027, eight clients' rates inside main's
spread (by id 136-147k a second against 126-145k); a subscription opened
to its seed of a row, its head naming its position, 1 000 opens a side in
each of four ABBA runs a build: over a database with a history of its own
245.2-251.2 us p50 against main's 250.6-254.6 (the root's, no history
written, 248.1-251.6 against 251.0-254.4), and sent after a change the
database holds already 247.8-252.7.

## Sealed archives

**An archive and a backup can be sealed** (`seal.rs`, `fennec key`,
`--key-file`). A copy in a bucket is out of reach of the disk's
encryption, so with a key every archive file and a backup file is
ChaCha20-Poly1305 (`crypto.rs`, RFC 8439's vectors, every case of
Wycheproof's, and 2 000 random cases against node:crypto): ChaCha20 over
AES-GCM because it has no secret-indexed tables in software, Poly1305 in
26-bit limbs. Each file is sealed under a key of its own, its frames under
random nonces with the tag before them, their place and last-ness in the
AAD (below); an image, the history and a tenant's presence are sealed whole
(`open_whole` refuses a file cut short), a segment a frame for each batch
of writes, whole records in each and a mebibyte at most, so a crash cuts
off a frame as it cut a record (`open_appended`, the segment's `sealed`
state). A frame's length is under no tag: changed, the bytes then read as
its tag are not it and the frame is refused, unless the length now runs
past the segment's end, which is a frame cut short there and nothing tells
from a crash's -- a verify then counts the bytes from it on as torn, as it
would in a segment whoever can write the archive had cut; so with the
magic's version byte set back to 2 or 1, or the tenant's name made longer,
which reads the first frame from further on, its length past the end: the
file one of no frame, as if cut after its head, a head no tag vouches for,
which no reader holds a file to until a frame opened
(`a_bit_changed_is_refused_unless_a_length_then_runs_past_the_end` walks
every bit of a file; the archive's own test changed its segment's middle
byte, wherever the run's flushes had put it, and failed the run that put a
length there). A file of the other kind than the archive is refused. About
400 MB/s either way on one core. The live file stays plain -- mapped and
read in place -- for the disk to encrypt. A checksum a record was measured
and not taken: hardware CRC32C was 45 ns of an 840 ns put at 530 bytes, 357
at 3 KB, under the write lock, and the browser has no hardware CRC.

**A file is sealed under its own key, and each frame's tag covers the one
before it** (`FENECSL\x03`, `seal::begin`, `seal::Frames`). A file begins
with the magic, its id -- 16 bytes of the system's randomness, refused
without it, as a nonce is -- and what it is (`seal::Binding`, below). Its
frames are sealed under a key of its own, HChaCha20 (`crypto::hchacha20`,
draft-irtf-cfrg-xchacha's construction and vector) of the id under a key
made from the key for that alone -- HKDF-Expand's of the key, never the key
itself (below) -- and each frame's AAD is the tag of the frame before it --
the first frame's, the file's whole head -- then its place and whether it
is the last. Three things 1.0's security review found in version 2 close
so:

- *A frame spliced across a cut.* Version 2's AAD was the file's id, the
  frame's place and its last-ness. A segment's last frames cut while its
  archiver was down, the archiver going on from the last whole frame sealed
  new frames at the places the cut ones had -- often batched otherwise, the
  lengths in the clear -- so an old frame k and a new frame k+1 opened
  together, writes dropped or doubled in the segment's middle. With the tag
  before it in its AAD a frame opens only after the frame it followed, and
  only a run of whole frames from the head does: the old ones or the new,
  which hold the same writes as far as both go (`position` goes on from the
  last whole frame's tag, `open_appended`'s `next`). What remains is a
  file cut at a frame's edge (below).
- *A nonce among every file's.* Every frame of every archive, backup and
  tenant took a random 96-bit nonce under the one key, with no count: a
  repeat -- which gives away the two frames' XOR and Poly1305's key for
  that nonce, every tag under it forgeable -- was a 2^-32 chance at 6.1e9
  frames, about 70 days of 1 000 frames a second, and 0.6% a year at 10^6
  a second across a node's tenants. Under a key of its own a nonce can
  repeat only among one file's frames: 2^-32 at 6.1e9 frames in one file,
  even odds near 3e14. A segment closes at 64 MB of writes, each frame a
  whole record of 8 bytes and more, so it holds at most some 2^23 frames,
  a chance below 2^-50; an image is a frame a mebibyte. Two files share a
  key only where their 128-bit ids do, 2^-32 at 2^48 files. The nonce stays
  random, never a count: a place is sealed again after a cut, as above.
- *A frame's length cast.* `seal_frame` wrote `plain.len() as u32`, and a
  segment sealed each batch of writes as one frame whatever its size: a
  `W` message carries a mebibyte of records at most (`replication::MESSAGE`),
  but a longer record goes alone, so a single record of 4 GiB -- a block's
  writes -- would have written a wrapped length, and nothing after it
  opened. Not reproduced, no test allocating 4 GiB: settled by reading. A
  frame is now a mebibyte at most (`seal::FRAME`, `Frames::seal` refusing
  more, its length checked), a batch going out in frames of whole records,
  and a record longer than a frame in frames of its own, beginning one
  (`Segment::write_by`). A crash between those leaves the record cut short
  after whole frames: `position` cuts it where it began, a frame's end
  (`seal::open_appended_to`), and `verify` counts it as the last segment's
  torn tail (`a_batch_goes_out_in_frames_of_whole_records`, at frames of
  256 bytes).

**A file's key is never made under the key itself** (`seal::FILE_KEYS`,
`Key::of`, `seal::file_key`). Version 3 as first made sealed a file under
HChaCha20(key, id), the id read from the file, while the same key seals
version 1 and 2 frames with ChaCha20 itself -- the one finding of 1.0's
review of version 3 that held it back. A ChaCha20 block is the 20 rounds
over the constants, the key, the counter c and the nonce N with that state
added back; HChaCha20 of c || N is the same rounds' words 0-3 and 12-15 with
nothing added back. So a block's keystream, its words 0-3 less the
constants and 12-15 less (c, N), is HChaCha20(key, c || N): bytes 0-15 and
48-63 of a block of a frame whose plaintext was known -- a string of 127
bytes and more written through the API before an upgrade, or a backup's
first block, its magic, change number and schema -- gave the key of a
version 3 file of id c || N, and whoever could write where archives are
kept, holding a version 1 or 2 file and no key, sealed a whole version 3
file -- any role, archive, tenant, change and rows -- that `unseal`,
`restore` and `restore --tenants` took. `an_older_files_keystream_gives_no_file_key`
does it with a version 2 frame of 64 known bytes: at 4a834627 its forged
image of tenant `xeno`'s at change 99 opened, holding what the forger
chose.

- *What a block under the key had for its input, in versions 1 and 2.*
  Counter 0 under each frame's nonce, Poly1305's one-time key
  (`aead_tag`), and counters 1 to the frame's length in 64-byte blocks
  under it, the keystream (`chacha20_xor`), wrapping at 2^32; the nonce
  12 bytes of the system's randomness -- before 0.2.0, where a descriptor
  ran out, SHA-256 of the clock and the process: never a value anyone
  chose. An opener runs block 0 under any nonce a file names, but only for
  a tag it compares, and opens no keystream before the tag matched: no
  block leaves but those of frames the key sealed. No code bounded a frame
  below 2^32 bytes: `seal_frame` cast its length to `u32` and sealed the
  whole batch, which a record of up to 2^40 bytes could be (`read_message`).
  A constant input whose counter is 2^32 - 1, the review's way, is out of
  their reach only because no frame of 256 GiB was ever sealed, and by the
  nonce's 96 random bits.
- *What version 3's own keys give away.* A file's key runs ChaCha20, so its
  frames' known plaintext gives HChaCha20 of the file's key, which makes
  no key; the key files' keys are made under runs HChaCha20 alone, never
  ChaCha20, so no keystream gives HChaCha20 under it away.
- *So that key is HKDF-Expand's* (RFC 5869, 2.3) of the key, `FILE_KEYS`
  its info, 32 bytes: HMAC-SHA256(key, info || 0x01), the key 32 uniformly
  random bytes, so no Extract. HMAC-SHA256 shares no rounds with ChaCha20:
  what ChaCha20 under the key gives away tells nothing of it, whatever a
  frame's length or nonce, and it is held to RFC 4231's vectors and
  Wycheproof's already (`the_file_keys_key_is_hkdf_expand` holds the
  first block to RFC 5869's SHA-256 cases, so a reader's own HKDF makes
  it). It is made once a key, as the key is read, and a file pays the one
  HChaCha20 it paid.

The derivation's vector, computed apart from this code in Python -- HMAC
and HKDF from its standard library, HChaCha20 written from the draft and
held to its 2.2.1 vector -- is `a_file_key_is_the_one_another_reader_makes`:
the key 00..1f, the id f0..ff, the info `fennecdb sealed file keys,
version 3`, the key files' keys are made under
`b334afb4e085eabd2c6cd511a3ee509a414695c8e2fd8d1f50845a65eb2e47da`, the
file's key `862970b95cd16be8c85c3e3f58f648a77eafd77a3a3a96a3aa0de9fdf0421c09`.

**A file says what it is, and every read holds it to that**
(`seal::Binding`, `Archive::learned`, `Archive::held`). The head names,
under the first frame's tag and so every frame's: the file's role (a
segment, an image, an archive's history, a tenant's presence, a backup),
the archive's id -- 16 bytes of the system's randomness made as the
archive's first sealed file is, and read back from its files' heads after
-- the tenant whose line it is (none for one database's), a segment's
first change, an image's change and time, and an image's archive's start
(`began`). Version 2 bound a file to its id alone, and a write's record
carries no change number -- `writes_in` counts its writes and `entries`
numbers them from the segment's name -- so whoever could write the bucket:

- deleted a middle segment and renamed the next into its name -- one
  beginning at an image's change plus one, which the hourly images make
  common -- and `verify` passed, naming a gap later than where the writes
  went, and a restore to a moment inside it gave a database missing a run
  of writes and holding later ones. A segment's head now says its first
  change, and a name saying another is refused
  (`a_segment_renamed_into_anothers_place_is_refused`);
- copied tenant X's directory, or a line of its moved aside, over tenant
  A's -- every tenant sealed with the one key -- and `restore --tenants`
  wrote X's rows into A's file, which A's tokens then read, or passed A
  over as gone. A tenant's line is read as its tenant's
  (`node_archive::Line::archive`, `Archive::of_tenant`): a file naming
  another is refused by name
  (`a_tenants_archive_copied_over_anothers_is_refused`);
- put an archive's image where a backup was asked for, or a backup in an
  image's place, and each opened as the other: an image has its primary's
  history, a backup a fork of its own. Each is now refused as what it is
  (`an_image_and_a_backup_do_not_stand_in_for_each_other`).

Every reader holds each file it opens -- `verify`, a restore to the end, a
change or a moment, `restore --tenants`, `consolidate`'s assembly, the
archiver going on (`position`, `ends_on_image_of`) and `unseal` -- and a
directory's files to one another: its sealed files' heads, read without
opening them, must name one archive and one tenant -- the line's, under
`--tenants` -- and their images one start, else nothing of it is read; a
head changed to agree opens nothing, its first frame's tag over it. What
is chosen from a head before its file is read is vouched for by that
file's first frame (`seal::vouched_in`): the archive's id and tenant, from
the first file that has one -- the history's few bytes as a rule -- and
`began`, from the first image's first mebibyte once it is asked for
(`Archive::began_of`). Read from the heads alone, every image's head
edited with the plain `began` chose another line for a restore to a
moment, or passed the tenant over as not yet archived, opening no image
whose tag would refuse it, and the archiver sealed its next images so; it
is refused now (`presence_and_began_edited_are_refused`). The rest need
only agree, each held once it is opened. A
segment cut inside its head, or after it and before a frame, vouches for
nothing and holds no write, and is read as one cut as it was made. A line
read as one database's -- `fennec verify <root>/<tenant>` -- holds its files
to one another, and `verify` says whose they are.

The plain files that choose what a restore reads are sealed or held to the
sealed ones. `began` -- when a line took its first image, written plain by
the prune that lets that image go, which chose the line a restore to a
moment read and whether one before it was refused or passed over -- is in
every image's head, and a plain `began` saying otherwise is refused by name
(`Archive::began`); lost, the images still say it. `presence` -- a tenant
gone and back, which marked a tenant gone, so passed over, by a line
written by hand -- is sealed as the line's other files are
(`Archive::note`, `Archive::presence`); one an archiver before this version
left plain is read only while nothing of its line is sealed in this
version, and an archiver's start seals it first (`Archive::seal_presence`).
So an operator who must mark a tenant gone by hand -- a node lost, its word
never heard -- does it in a plain archive by a line in `presence` as
before, and in a sealed one leaves the tenant's file out of a restore's
output (or moves its lines out of the root): no tool writes a sealed
presence but the archiver.

What none of this covers, and the docs say: a file cut at a frame's edge
opens as the shorter file, which nothing tells from a crash's -- the
archive's own order notices it in any segment but the last (a gap no image
ends, refused; a gap an image ends, named by `verify`), and `verify` says
where the last ends; a middle segment deleted is such a gap; and an older
whole file, or a whole archive or line, put back opens as what it was, the
names and heads holding a file to its place, never to its age. A bucket
that keeps versions or locks objects is what holds an archive to its age.
A plain archive promises nothing against whoever can write it.

*Rejected*: the file's name or path in the AAD, which a sync tool's copy, a
renamed backup or a node archive's directory moved would break where a
sealed file opens wherever it is copied -- the head names the numbers a
name carries, and a tenant, and a reader compares them with the name it
found; a file's key from HKDF alone, an HMAC -- four SHA-256 blocks -- a
file where HChaCha20 is one block's rounds (the key HChaCha20 runs under
is HKDF-Expand's, made once a key); HChaCha20 of the key itself under an
input of its own, the review's way -- a counter word of 2^32 - 1, which a
ChaCha20 block of an older frame reaches only past 256 GiB -- whose
separation rests on no such frame ever sealed and on the nonces'
randomness, where HMAC's rests on nothing a frame did; the frame's place as its nonce,
which a cut sealed again; a tag over the whole file, which an appended
segment cannot keep without writing it again; an id counted by the archive,
which two archivers or two backups under one key would share; a sealed
manifest of an archive's files, which would hold it to its age only where
the manifest itself could not be put back, and an archive's directory can.

*Older files.* Version 2 (`FENECSL\x02`, the id after the magic and every
tag over it and the frame's place, the frames under the key itself, main's
builds from 2026-11-15 and no release) and version 1 (`FENECSL\x01`, no id,
every release from 0.1.7 to 0.2.0) open as they did (`tests/sealed-v1/` and
`tests/sealed-v2/`, an archive and a backup each version's code sealed,
held by `older_sealed_archives_still_open`). Nothing is sealed in either
any more, and a segment an older archiver began is gone on in by none: the
archiver cuts its tail, durably, and its next write begins a version 3
segment after it, and after an image of version 3 of the line's end
(below) -- frames of two versions in one file would open as neither, and
going on in an older one sealed writes bound to nothing. Their
files bind nothing a reader holds them to, so the binding holds only for a
key that never sealed one: whoever holds version 1 files under a key builds
others from their frames, and a version 1 or 2 file of any archive under
the key -- a segment, an image -- reads where it is put, but past a line's
first file of version 3. There an older image or segment is held to where
it stands in the archive's order (`Archive::older`): before the first
image or segment sealed in version 3, by its name -- an image of an
earlier change, or of its change taken earlier, a segment whose writes all
come before it -- it reads; at or past it, it is refused by name. Tenant
xeno's image from before an upgrade, copied into acme's line under a later
change, and xeno's segment named before acme's first image with its writes
running on past it, each restored as acme's at 4a834627 (at changes 6 and
5, `older_files_put_past_a_lines_newer_ones_are_refused`); each is refused
now, and so is a presence sealed in version 1 or 2, which no archiver
wrote.

That first file is an image: an archiver going on in a line an older one
sealed -- or a line of no file of version 3 -- seals an image of version 3
of the line's end before its first segment of version 3
(`Archive::seal_before_version`), assembled from the line's files as the
hourly round takes one and timed after every image the line holds:
one database's archiver as it goes on, a node's for each tenant as it
starts, on the thread beside its stream (`NodeArchive::seal_ends`,
`Archive::seal_end`), or at the tenant's first write where that comes
before the tenant's turn, a write meanwhile waiting for the image being
sealed beside rather than assembling a second (`Archive::sealing`).
Sealed at a tenant's first write, as the second round of this review
made it, a line no write reached after the upgrade held no file of
version 3, and an older image put in meanwhile -- of its end's change
under a later time, or of a later change -- was what that image was at
last assembled from, its rows sealed in and read by every restore past it
(1.0's third review of version 3; at 0abe1d44 the idle line of
`an_idle_lines_upgrade_image_is_sealed_as_the_archiver_starts` held no
image of version 3 20 s after its archiver started). A line moved
aside is not sealed: it takes no write, and a restore to a moment reads
it only before the line after it began, where an image timed at the
upgrade is never the base. Begun with a segment, as
version 3 was first made, the line's first file of version 3 left before
it, in the archive's order, an image of the change before that segment
taken at any time: an older image of that change -- another tenant's or
another database's under the key -- copied in under a later time was the
base of every restore past the upgrade until the first round's image, an
hour on by default, which then sealed it in (1.0's second review of
version 3; at 9865040b a tenant's restore to the end held another
database's notes for 6 of its 8, and with such an image of the change
before it for 5 of its 8, `an_older_image_put_in_after_an_upgrade_is_no_restores_base`).
Now such an image of the end's change stands at or past the upgrade's
image, and is refused; one of an earlier change stands before it, and a
restore past the upgrade starts from it or a later image. Where a line's
first file of version 3 is a segment -- that image let go of by `prune`,
which lets go of every image before it first, or a line with no image --
an older image is refused wherever it stands: put in before that segment
it was the base of a restore to a moment the line no longer held, which
was refused, that segment's writes read on it (at 9865040b a restore to
the change after it held another database's notes for 6 of its 7,
`an_older_image_is_read_only_before_an_image_of_version_3`). A line's
first segment of version 3 that a crash left holding no write, the
image's name lost with it, is removed as the archiver goes on, and the
image sealed then (`an_upgraded_line_goes_on_from_an_image_of_its_end`).
Where a file of version 3 stands is noted once it is in place
(`Archive::landed`, after an image's rename and a segment's create), never
as it is bound: noted so, an image whose write then failed -- a full disk,
on Windows a scanner holding its side file -- left the line taken for one
whose first file of version 3 was that image, so the archiver going on on
the same archive, as `follow` does, sealed none and began the line with a
segment, and after a restart every older image was refused and no restore
past the upgrade had a base (1.0's third review; at 0abe1d44
`an_upgrades_image_that_failed_to_write_is_sealed_as_the_archiver_goes_on`'s
restore to the end was refused: `<image>: an image of change 1, sealed
before version 3, where the first file this archive holds of version 3 is
its segment beginning at change 5, and an older image is read only before
an image of version 3`). The id an archive's first sealed file is made
with is kept as it is made, written or not, rather than the identity
cleared on a failed write: the round beside the stream binds files of the
same line, and one bound under the id before the failure and landed after
a rescan had made another would leave two archives' files in one
directory, which every read refuses. An end that does not restore is
refused as the line's damage before anything of version 3 is written,
naming the ways out: put right what `fennec verify` refuses in it, or
begin a new line, keeping this one for restores to earlier moments -- one
database's by archiving into a new directory, a node's tenant's by moving
its directory aside as `<tenant>.<any name>`, the archiver stopped, after
which the archiver begins the new line with the node's image. One
database's archiver stops on it, a node's passes over that tenant
(`an_upgraded_line_whose_end_does_not_restore_is_refused_as_damaged`). The
image is one assembly a line, once: what the first round would have taken
of it within the hour, about 9 s a GB of database
(`archive::tests::upgrade_image_cost`, by hand in a release build: a line
of 262 144 notes, a sixteenth of them in a segment after its older image,
its image of 266.6 MB assembled, opened, imaged and sealed in 2.06 to 2.38
s over three rounds, 7.7 to 8.9 s a GB, on an 8-core arm64 Mac at a load
of 2.5 to 2.7). One database's archiver spends it on its stream, before it
takes the next write: its primary fills the stream's socket meanwhile,
then waits for it at most `SENT_WAIT` on one durable write, marks it
lagging and answers the writes after without it until it catches up, and a
send held up past `SILENCE` ends the stream, which the archiver makes
again from its end. A node's spends it beside its stream, where the stream
waits only for a tenant written before the pass reaches it. A line this
version's first builds upgraded -- its first file of version 3 a segment;
no release and no build of main wrote one -- reads none of its older
images: restores from its first image of version 3 on, and its archiver,
go on; a `verify`, a restore before that image and, until it holds one,
its round refuse each older image by name, until they are moved out.

No upgrade puts an older file where it is refused. The review read one
as able to: a primary behind its archive sending an image of an earlier
change at the first hello after the upgrade, leaving an older segment
holding writes past a version 3 image. A hello's image of a change before
the archive's end leads from no history the archive is on, whatever its
lineage (`History::continues`), so it is not taken (`Took::Diverged`) --
one database's archiver stops, a node's moves the line aside -- before
anything is written (`a_hello_behind_an_older_line_leaves_it_as_it_was`).
So a refusal names the file and the way out: `<file>: an image of change
7, sealed before version 3, at or past its image of change 7 taken at
<ms>, the first file this archive holds of version 3: no archiver puts it
there, so it is not this archive's. Move it out of the archive's
directory, and the archive is read without it`.

What remains: an older file standing before a line's first of version 3
-- its images and segments before it, which an older file of another line
can be put among, read by a restore to a moment before the upgrade, and
those put in before the upgraded archiver sealed that image, from which it
was assembled -- and every file of a line with none of version 3 -- a line
a node's archiver moved aside before its upgrade, or one whose version 3
files were deleted -- which reads as an older whole line put back does. A line's history sealed in version 1 or 2
reads wherever it is, in place of one of version 3 too: it has no place in
the order, and is read by the archiver going on and by `verify` alone,
never a restore, so all it decides is whether a primary's hello is taken
as leading from the archive. Neither pruning
the older files nor restoring a backup and sealing it again under the same
key ends that -- they are wherever they were copied -- so the way out is a
new key (`fennec key`) sealing everything from then on and a new archive
begun under it, the older files read with the old key while they are kept.
A switch refusing them is not built: under it every archive and backup
0.2.0 sealed would be unreadable. A version past 3 is refused by its number
(`sealed as version 4, which this binary does not open (it opens 1 to 3): a
later fennecdb's`), never read as a plain file: `is_sealed` reads the
magic's first seven bytes, and a role a version 3 head names that this
binary does not know is refused by its number too. A version 3 file is
refused alike by the binaries before it: main's version 2 builds say
`sealed as version 3, which this binary does not open (it opens 1 and 2): a
later fennecdb's`; 0.2.0, which took the magic's whole eight bytes, finds no
sealed file -- `fennec restore today.sealed out.fennec --key-file k` says
`today.sealed: not a sealed file`, `fennec verify` or `restore` of an
archive `<image>: not sealed, where the archive is`, and without a key an
image does not open as a database (`corrupt: invalid fennecdb signature`).
The binaries reading an archive are upgraded before the one writing it.

*Cost*: a file's own key is one HChaCha20 -- a ChaCha20 block's rounds --
as it is begun and as it is opened, and the key it runs under one
HMAC-SHA256 a process, as the key is read (`Key::of`); its head is 66 bytes and its tenant's
name where version 2's was 24, and its first frame's AAD the head, three
Poly1305 blocks more than version 2's and one more for each 16 bytes of
the name; every frame after takes 25 bytes of AAD, the tag before and its
place, as version 2's took the id and its place: nothing more a frame,
and not one byte more a write -- but a batch past a mebibyte, now split,
32 bytes a mebibyte. Measured in one release binary, version 3 against
version 2, each the best of three rounds taken in turn
(`seal::tests::seal_cost`, run by hand), on an M1 shared with others whose
load was 2.9 as it began and 2.0 as it ended (a run beside a load of 9.5
was discarded): 10 000 frames of 1 KiB, a thousand a file, 1.00; of 1 MiB,
1.00 (26.1 s against 26.0, some 400 MB/s); and 10 000 files of one 64-byte
frame each 1.86, 1.0 µs more a file -- the subkey, the longer first AAD
and, on version 3's side of the loop alone, the system's randomness for
the id, which version 2 drew as it began too. Measured with version 3's
first derivation, HChaCha20 under the key itself: a file now runs the same
HChaCha20 under the key HKDF-Expand made once, in `seal_cost`'s loop as in
an archive's. An archive's files are an
image an hour and a segment each 64 MB: nothing a write pays. The seal is
the `fennec` binary's alone -- `fennec-server` reaches no archive and no
seal, and fennec-http is not in the browser module -- and its code grew
by 18.9 KB (`__text` 2 850 944 bytes built from `train-d`, 2 870 272 with
version 3, profile `cli` on macOS arm64; the file 32.4 KB larger): the
binding written and held by every reader, the vouched heads, the chained
frames and the split batch.

**A frame's nonce is the system's randomness, never a fallback**
(`crypto::system_random`), and so is every key and nonce fennecdb makes:
`fennec key`'s, the hooks' stand-in key, SCRAM's client nonce, a sealed
file's id and an archive's. Each was
read from `/dev/urandom`, and where its open failed -- at a busy server's
descriptor limit, EMFILE -- the nonces fell back to SHA-256 of the clock,
the process id, a stack address and a count that began again at each call:
two frames sealed in one tick under one key could take one nonce, which
gives away the XOR of their plaintexts and Poly1305's one-time key, so
both tags could be forged. The randomness is now read with no descriptor
(`crypto/unix.rs`) -- `getrandom` on Linux (glibc 2.25, musl, the kernel's
3.17), `getentropy` on macOS, 256 bytes a call -- and where the system
gives none a seal fails as a write does (`Frames::seal`'s error, the
archive's), a key is not made and a SCRAM sign-in is refused. A nonce is
one call where it was an open, a read and a close: 1.0-1.2 us against
10.7-13.8 on an M1, 200 000 draws three rounds each. A test runs
it with every descriptor taken, and with the system refusing
(`crypto::without_randomness`). `/dev/urandom` stays only where the call is
missing or denied: a kernel before 3.17 (ENOSYS), a seccomp filter that
answers `getrandom` with EPERM while the file stays readable (reproduced in
Docker with a profile returning errno 1; std and the getrandom crate read
EPERM as ENOSYS too), and a Unix neither Linux nor macOS, which no release
is built for; where the file cannot be read either, the seal is refused. Only ids, which must be unique and need not
be secret -- request and trace ids, a hook set's version -- may take the
clock where the system gives nothing (`crypto::id_u64`, a count of the
process's own in place of the one that began again).

**The key file is warned of where others can read it** (`seal::open_key`,
by `conn::open_to`'s rule, `--tls-key`'s): its name and mode on stderr as
`Key::from_file` reads it, on Unix alone, never a refusal -- a container's
secret volume mounts its files 0644 -- where whoever reads it opens every
archive and backup it sealed and seals files that open as theirs. Held
through the command (`fennec-cli`'s `a_key_file_others_can_read_is_warned_of`:
`fennec verify --key-file` of a file of mode 0644 says so, of 0600 says
nothing), so a `Key::from_file` that dropped its log fails it, as the
seal's own test of `open_key`'s words did not.

**A line read as one database's says whose it is.** Only `--tenants`
holds a tenant's line to its directory's name (`Line::archive`), so a line
copied where one database's archive was -- or a node's tenant's moved
there -- restores with `fennec restore <dir>` as that database: its files
agree with one another, and a database's archive names no database. `fennec
verify` and `fennec restore` of such a directory say ``sealed as tenant
`x`'s archive`` (`Verified::tenant`, `Restored::tenant`,
`a_tenants_line_restored_alone_says_whose_it_is`); one database's says
nothing, and SECURITY.md says one database's archive is bound to no name.

## follow

**`--follow` confirms nothing that is not on disk.** `fennec import --follow`
reads a logical replication slot through `pgoutput` and applies every change
through the copy's own mapping (`fennec-import/src/follow.rs`). The slot's
confirmed position only moves past a transaction an fsync has covered, and
every write is a put or a delete by id, so a broken stream or a killed
follower resumes from the slot and replays what it had applied without
changing it. The slot is made before the copy is read, so the stream starts
with changes the copy may already hold; the same idempotence converges them.
A `_follow` collection in the file records whether a collection's copy
finished: a copy cut short is made again rather than streamed on top of,
which would lose the rows it never reached. An update arrives without its
TOASTed columns -- a `vector(768)` is 3 KB, past the threshold -- so the
follower takes them from its pending writes or the collection; flushing
before each such read cost the batching, 5 900 rows/s against 17 100. The
pending writes are put at `--batch` changes or once their rows hold
`BATCH_BYTES`, 16 MiB, counted as a copy's batch is
([crates.md](crates.md#a-sqlite-files-lengths)), each document moved into
its statement: put at `--batch` changes alone and copied into the
statement beside the pending ones, a transaction of a 4 MiB row and 2 000
of 28 KiB held 377 MB at once, and holds 97 MB now, under five times
`BATCH_BYTES` and its largest row
(`a_follower_holds_its_writes_by_their_bytes_not_their_count`). Commit
to visible: p50 0.16 ms (`make follow-bench`). `fennec-server --follow` runs the
same follower on a thread of the server's, over the database it serves, so
the mirror is served over HTTP and subscriptions with no second process
over the file -- two corrupt it (`fennec-server/src/mirror.rs`). The
importer's options come as `--follow-index` and the rest
(`fennec_import::args`, which `fennec import` reads its own with too). The
collection takes no write but the follower thread's (a write hook, 403):
the next change from PostgreSQL would write over one, or a copy made again
forget it. The follower stops at the server's shutdown flag, and the
shutdown waits for it (`durability::before_shutdown`, 5 s at most; a copy
still being made stops within `client::LOOK` and is left for the next
start, below) before its checkpoint;
an error it cannot wait out stops the server rather than leave it serving
a mirror that no longer moves -- through the one shutdown, exit code 1
(`durability::stop_with`, [durability.md](durability.md#the-one-shutdown)). This is why the PostgreSQL client is
`fennec-wire`'s, below the importer: in the server it made the importer
depend on the server, which could then not run it. A message's length is
held to PostgreSQL's gigabyte (`proto::MAX_MESSAGE`, its MaxAllocSize)
before anything is allocated, and under it the body's buffer is the
length believed, allocated so its failure is an error (`proto::zeroed`,
as the router's `zeroed`, and for its reasons: no copy at any size, a
lying length costing the pages its bytes reach): a length from the
server, or from anyone on the path (under any `sslmode` short of
`verify-full`), that a system
refused -- one that does not overcommit, Linux's heuristic on a machine
smaller than the gigabyte -- aborted `fennec import`, and `fennec-server
--follow` with everything it served, and is a broken stream now, which
the follower connects again after. The counts a message carries are held
to it too: a column count is an Int16 never negative, read unsigned
(`be_count`: read signed, `0xFFFF` sized a `Vec` by `usize::MAX` and
panicked), and a TRUNCATE's relations are given room for what the
message holds, where its count alone asked for 16 GiB. The reader is the
code it was, `calloc` either way, the loop reading messages the same
instructions in other registers once the refusal's message went out of
line (`refused`: formatted in place, the length was stored to the stack
at every message): over messages in memory, twelve runs a side in ABBA
turns, 120 B 30.2 ns a message against 30.3, 3 500 B 248.4 against
250.0, 64 KiB 8.53 us against 8.61, 2 MiB 58.5 against 58.6. A subscriber hears a PostgreSQL
commit 0.09 ms after it returned at the median, a row inserted with a
vector under HNSW 0.19 ms, p99 4.0 and 3.3 (`make mirror-bench`; the code
before the pg wire went gave 3.7 and 3.7 the same day); a server killed
while the table was written to held every row 370 ms after it started
again. The
follower adds 178 KB to `fennec-server`.

*The copy waits on PostgreSQL with no lock held.* `ensure_copy` decides
under the write lock -- the `_follow` entry read, a stale collection
dropped, the entry written unfinished -- and lets it go; the dial, the
sign-in and the COPY are read with none, and the rows land a batch at a
time, each batch, index and the checkpoint a statement of its own under
the lock (`load::run_locked`, `Land`), the entry marked finished after the
last. Every release with a server that follows held the write lock from
the dial to the last row: `fenec-pg --follow` (v0.1.5, v0.1.6),
`fenec-server --follow` (v0.1.7 to v0.1.12) and `fennec-server --follow`
(v0.2.0), so every request of the server, of any collection, waited on
PostgreSQL for as long as the table took to come -- and for good where
whoever answered at its address sent a byte a little under every 30 s, a
read's timeout; the shutdown takes the same lock, so a SIGTERM never ended
that server either. The dial took std's connect (the system's 75 s and
more, the name's lookup unbounded) and every read of the sign-in 30 s
anew. Now the sign-in is held to one deadline, libpq's `connect_timeout`
(30 s by default; `client::CONNECT_WAIT`): the name looked up on a thread
of its own (`fennec_http::dial::resolve`), each address dialed within what
is left, the TLS handshake (`dial_within`) and every message to the first
`ReadyForQuery` read through `Until`, PBKDF2's own time added back, since
it is this side's. The COPY's socket has a read timeout of `client::LOOK`,
200 ms, and its reads (`Watched`) ask again at each until the stop flag is
set or the server has said nothing for 30 s: a stop or a shutdown ends the
copy within 0.2 s wherever it waits, the copy left unfinished as a kill
leaves one, made again from the start at the next start. A reader sees the
collection fill a batch at a time, its `_follow` entry unfinished until
the end -- what a reader saw before was nothing at all until the copy was
whole, its request waiting the while. A failed copy drops what it landed,
under the lock, as before. The cost is the lock taken once a batch
(`--follow-batch`, 2 000 rows by default) where it was taken once a copy,
and a 200 ms timer on the copy's socket, which a busy copy never meets:
a copy's rows are read and put as they were. fennec-server's
`a_copy_held_up_holds_up_no_request_nor_the_shutdown` holds it at the three
places a stand-in stalls -- a startup never answered, a TLS handshake
begun and never answered, a COPY sending a byte every half second --
each time a write and a read of another collection answered at once and
a SIGTERM ending the server, exit 0, within its wait.

*A keepalive's end is confirmed only where the server is proven.* With
nothing pending, the follower confirmed the end a keepalive named: the
walsender's word for how far its log had gone, which lets a quiet
publication's slot move with the log other tables' writes fill --
PostgreSQL 15 and later skip an empty transaction, so with no change of
its table the slot hears nothing else, and its own subscriber reports the
position received where nothing is pending (`send_feedback`) for that
reason. But a keepalive is anyone's on a path no CA checked, and an end
named past what the stream reached, confirmed, has PostgreSQL skip every
change before it for good: PostgreSQL 17.12, sent a confirm a terabyte
past its log's tip on a slot's stream, set the slot's
`confirmed_flush_lsn` to it, and a row inserted after never came on the
next stream, where one inserted before the confirm did. `pg_recvlogical` never confirms a keepalive's
end: its written and flushed positions are the last XLogData's start, put
on disk. The follower now does as PostgreSQL's subscriber does only on a
proven stream (`WalStream::proven`: this machine, TLS checked against a
CA, or a sign-in bound to the session, `SCRAM-SHA-256-PLUS`), and as
`pg_recvlogical` does on any other, where a quiet table's slot keeps the
server's log until its next change -- the price of a stream someone may be
writing. A commit's end is confirmed once on disk on any stream, as
`pg_recvlogical` confirms what it wrote: on one not proven, whoever can
write a keepalive can write a commit, and the changes before it, which no
rule of the follower's tells from the server's -- only `verify-full` and a
bound sign-in keep that writer out, which is why they are the default
off this machine. Every release with `--follow`, v0.1.5 to v0.2.0, confirmed any
keepalive's end. fennec-import's
`a_keepalive_moves_no_confirmed_position_on_a_stream_not_proven` sends one
naming a terabyte past the commit, over plain text to a name off this
machine (`dial::stand_in`), and holds no status update to it; over
loopback it is confirmed.

*What the client keeps is bounded by what it asked.* A simple query's
answer is held to 100 000 rows (`QUERY_ROWS`; the client's queries read a
catalog for a row or a few, the follower's read-back one row, `limit 1`)
and to `MAX_MESSAGE` of what the rows cost here, 24 bytes a row and a cell
beside their bytes: a null cell is 4 bytes of the server's and 24 of
memory, so a server's answer of a gigabyte of rows held six. A COPY's line
is held to `MAX_MESSAGE`, PostgreSQL's bound of a row, and each byte is
scanned for its end once (`CopyOut::scanned`): a line sent a byte a
message was scanned whole at every message, quadratic, and kept without
bound. Every release since v0.1.0 kept both without bound and scanned so.

*A follower that stops lets its slot go before it returns -- within a
bound, and on a clean stop alone.* It ends the stream as the protocol ends
one (`WalStream::end`): `CopyDone`, then the server's messages read up to
its `ReadyForQuery`, what it still sends of the stream dropped unapplied
and so unconfirmed. The walsender calls `ReplicationSlotRelease` before its
`CommandComplete`, so the slot is free once the answer is in. The follower
used to send `Terminate` and close, which left the slot held until the
walsender next read its socket -- which it does not while it decodes a
transaction, any table's, published or not: a slot streaming from far
behind, or one whose walsender was busy with another table's bulk load,
stayed held for as long as that took. A drop of the slot just after --
what the follower's own note says to do to stop for good -- was refused,
`55006`, "active for PID", and a follower started again at once met its
own slot taken and waited out a retry. CI's `postgres` job met it in 2 of
its first 9 runs (`a_copy_cut_short_is_made_again`, whose second follower
stopped while its walsender decoded the other tests' writes since the
slot's last confirm); beside two busy loops in PostgreSQL's 3-CPU
container the suite failed on it first at run 27, once in 109. Every
release since `--follow` came has the bug: v0.1.5 and v0.1.6 (`fenec
import --follow`, and the server's as `fenec-pg --follow`) through
v0.2.0 (`fennec-server --follow` from v0.1.7) drop the stream with a
`Terminate` alone. It loses nothing; it is fixed on main.

*The bound.* `follow::END_WAIT` is 3 s, and every read of the wait is held
to what is left of it (`client::Until`): before a read that would go to
the socket, the socket is waited on for the rest (a peek, or rustls taking
whole records in), so the rest of a message begun, and bytes already
buffered, no longer wait out the socket's own 30 s timeout a read -- the
first form of the end looked at its deadline before each message only. The
`CopyDone`'s write is held to it too, its socket's write timeout set to
what is left before each try, and the `Terminate` of the close after it is
sent only where the socket takes it at once (`Client::drop`, non-blocking):
at a full send buffer -- a server reading nothing -- the drop's waited a
whole wait more, twice the bound, and on a failure it waited with no bound
at all, before `fennec-server` could store its exit code; the close tells
the server as much once it reads. 3 s and
not 10 because a stop has to have one bound it keeps: `fennec-server`'s
shutdown waits `STOP_WAIT`, 5 s, for its follower to settle and end its
stream, so at 10 s the shutdown cut off any wait past its own 5 s, the
stream closed by the exit, and the follower's bound never held there; at
3 s the settle's fsync has 2 s before it, which `mirror.rs` asserts at
compile time (`END_WAIT` + 1 s < `STOP_WAIT`). `STOP_WAIT` counts from the
shutdown's start, as the follower's stop does -- it looks at the same
flag, within its 200 ms `IDLE_WAIT` -- not from its own: the shutdown's
waits before its final sync run one after another, `HELD_GRACE` 2 s,
`END_GRACE` 0.5 s, the traces' flush 2 s where tracing is on, then the
follower's, and counted from its own start that came after theirs: with
`--follow` and tracing the waits came to 9.7 s with the syncer's `LOOK`,
0.3 s of docker's default 10 s before its SIGKILL left for the final sync
(the first form of this text said 7.5 s, leaving the traces out). Counted
from the shutdown's start the final sync begins 5.2 s after the signal at
most, the rest of the 10 s left to it and the checkpoint, whose cutting
off costs the graph's image, never a write (the repository's
`docker-compose.yml` gives 30 s). `deploy/` runs no `--follow` unit: its servers are `--dir`
nodes (`TimeoutStopSec=120s`), which `--follow` refuses, and its routers
are `fennec-shard`. `fennec import --follow` keeps this bound and no other:
before the wait the rest of a transaction is applied and its fsync made,
and after it the checkpoint rewrites the whole file, none bounded by a
time, so a large transaction or file outlasts docker's 10 s. A SIGKILL
among them loses nothing confirmed -- the follower confirms only what is on
disk, and the checkpoint is a side file renamed into place -- and leaves
the slot to be let go when the walsender next reads. What 3 s waits for:
200 000 rows of a table the slot does not publish, decoded through disk,
held a stop 0.2 s, a million in memory 0.5 to 2 s on a loaded machine; a
longer transaction is not waited for. Past the bound, or once cut, the
follower closes the connection and says so in one line (`Event::SlotHeld`,
the shell's `warning`, the server's `follow:` log line): the slot still
held, which PostgreSQL releases when its walsender next reads the
connection, a drop meeting `55006` until then, which a follower started
again waits out (`retryable`). Where the wait ended otherwise -- the
connection closed or failed, or the server answered with an error
(`client::Unended::Failed`) -- the line says the slot *may* still be held:
a walsender that exits or fails lets it go, a proxy's close or a network's
failure need not have reached it. The shell's second ctrl-c (`stop::AGAIN`)
ends the wait within 50 ms (`client::CUT_LOOK`), the steps before it -- the
rest of a transaction being applied, the settle's fsync -- and the
checkpoint after it still run.

*A signal is no failure.* Linux never restarts a `recv` on a socket with a
timeout, `SA_RESTART` or not (signal(7)), and every socket of the wire
client's has one; `fennec import --follow` runs its follower on the main
thread, where a SIGINT or SIGTERM sent to the process lands, and an idle
follower spends nearly all its time in the 200 ms peek of `WalStream::next`.
That peek returned `EINTR` as an error, the stream was taken for broken --
no `CopyDone`, the slot left held, no warning -- and `run` printed "stream
Interrupted system call (os error 4); again in 500.0ms" before it saw the
stop. macOS restarts the call, so no run here showed it; a reviewer gave
the sockets Linux's behaviour with `siginterrupt` and signalled the
follower's thread, and it failed 3 times in 3; on Linux itself (musl in a
container, the handler installed with `SA_RESTART` as the shell's is, no
`siginterrupt`) `a_signal_is_no_failure_of_the_stream` failed 3 times in 3
with the fix's `Interrupted` arm turned off and passed 3 times in 3 with
it. `Conn::readable`, the one
wait under the idle peek, the end's reads and TLS's, now goes on after an
interruption for what is left of its wait, never failing and never
starting the wait again. Every other call of the client's: `connect` is
std's, which asks again on `EINTR`; every read of a message is `read_exact`
and every write `write_all`, which ask again; a TLS flush asks again
(`Conn::flush`); the end's write and reads go through `Until`, which goes
on for what is left; the drop's `Terminate` is non-blocking and so never
interrupted; the TLS handshake is rustls's `complete_io`, which asks a
read again, its sends restarted by the kernel (the handlers are installed
with `SA_RESTART` and the dial sets no send timeout); the SCRAM nonce's
`getrandom` asks again. The client accepts nothing. Beside it, nothing on
the stop's path takes a signal for a failure: std's `sync_data` and
`sync_all` ask `fsync` again, `thread::sleep` sleeps out what is left, the
checkpoint's file writes are `write_all` and its rename is not
interrupted, and the lock is taken with `F_SETLK`, which never waits.

*A failure waits for nothing.* The first form ended the stream when the
follower stopped on what it read too (a dropped column, a storage error),
up to 10 s, before `run` returned -- and `fennec-server` stores its exit
code (`exit_with(1)`) only when the follower returns. A shutdown already
waiting on the follower (a SIGTERM while the walsender decoded a large
transaction and the final settle's fsync failed; or a SIGTERM in the
10 s after a failure) ran out of its `STOP_WAIT` first and exited 0, with
a checkpoint (`checkpoints`), and with no SIGTERM the server served a
mirror that had stopped moving for those 10 s more. Now only a clean stop
ends the stream so; a failure closes it (its `Terminate` sent only where
the socket takes it at once) and returns at once, and the mirror stores
its code before its log line and before it lets the shutdown's wait go.
A panic of the follower's thread goes the same way: `fennec-server`'s
release profile unwinds, and the thread's end let the shutdown's wait go
and nothing more, so the server served a mirror that no longer moved and
exited 0 at its next shutdown, with a checkpoint; the mirror's guard
(`mirror::Ending`), dropped on an unwind too, stores the code first. On an
error of the connection's the stream is not ended either: the connection
is what broke. A follower killed outright cannot end its stream; one
started meanwhile meets `55006` and retries until the server notices the
connection gone.

*The tests.* Against stand-ins, every run and every time: fennec-import's
`a_stopped_follower_returns_once_its_slot_is_let_go` (applied, confirmed,
`CopyDone` -- never a `Terminate` alone -- and back only after the
server's answer), `a_follower_that_fails_returns_before_it_waits` (no
`CopyDone`, an answer that never comes not waited for: 3 s in that test
without the fix), `a_second_stop_ends_the_wait_and_says_what_holds_the_slot`,
`a_stream_the_server_closed_says_the_slot_may_be_held` ("still held"
before the wording) and `a_signal_is_no_failure_of_the_stream` (SIGUSR1 to
the follower's thread every 5 ms, its calls made to fail with `EINTR` as
Linux's do, from its first confirm through the stop: the stream failed
3 times in 3 with the peek's `EINTR` returned); fennec-wire's
`ending_a_stream_is_bounded` (silent, still sending, and stopped in the
middle of a message with a whole one buffered before it -- 30 s a read
without the bound) and its TLS twin,
`a_signal_neither_fails_nor_lengthens_the_end_of_a_stream` (signals every
2 ms through the end; it passed before too, `read_exact` asking the
interrupted read again against the same deadline, and holds what `Until`
now does itself), and, over a server that reads nothing with both buffers
full, `a_full_send_buffer_holds_the_end_no_longer_than_its_wait` and
`a_drop_never_waits_for_the_server` (both failed before: the end took
twice its wait, and the drop waited until macOS made room some 5 s later);
fennec-server's
`a_follower_that_fails_stops_the_server_with_its_code_before_it_waits`
(a stand-in PostgreSQL whose walsender sends a change the follower cannot
read and never answers a `CopyDone`, SIGTERM sent as the follower ends
its stream: exit 1, no `CopyDone`; with the first form, exit 0),
`a_stopped_server_lets_its_slot_go_before_it_exits` (its log read to the
process's end, the shutdown's own line in it, before it is held to having
no "held at the stop", which the line saying the slot is held and the one
saying it may be both say), and the mirror's
`a_follower_that_panics_stops_the_server_with_its_code` (exit code 0
without the guard) and
`the_wait_for_the_follower_counts_from_the_shutdowns_start` (5 s counted
from its own start, under 3 allowed). Against PostgreSQL,
`a_stopped_follower_lets_its_slot_go` stops a follower once its walsender
has read up to the commit of 200 000 rows of a table the slot does not
publish, decoded through disk (`logical_decoding_work_mem` at its least),
and looks at the slot as the follower returns. It is the end-to-end check,
not the guard: the walsender is busy 159 to 198 ms after that commit and
the follower notices its stop at a random point of its 200 ms
`IDLE_WAIT`, so without the end it passed 5 to 20 % of runs on the
review's Mac, more on faster hardware. With the end the suite passed 153
times in 153 under load. The test has a database of its own: every slot
reads the whole server's log, but one of another database passes over
these rows at a glance (30 ms for half a million) where one of the same
database decodes them (550 ms) -- its first form, a million rows beside
the other tests, held their convergence past its 20 s on a runner whose
walsenders were off the CPU six tenths of the time. `follow::` takes 3.28
to 3.66 s, 3.44 at the median, over 30 runs in a row on a shared Mac (load
average 2.2 to 6.1), where main's took 3.5 to 3.6; the new test 0.9 s
alone. The tests drop each slot right after its follower stopped, and
cut only their own slot's walsender, by `pg_replication_slots.active_pid`:
by `application_name`, `a_broken_stream_loses_nothing` cut every follow
test's stream running beside it; their leading drop of a slot left by an
earlier run is `DROP_REPLICATION_SLOT ... WAIT`, the server's own wait.
fennec-server's `a_server_stopped_or_killed_goes_on_without_losing_a_row`
finds the slot free after a SIGTERM, but its walsender is idle and the
look comes 100 ms after the exit, so it holds only that the stop leaves no
stream open.

TLS to the source (`sslmode`, `fennec_wire::client::SslMode`) is asked
for as libpq asks: an `SSLRequest` before the startup message, so the
user's and the database's names, and SCRAM's exchange, go out only once
the handshake is done -- and under `verify-full` only to the server the
certificate names. The modes are libpq's five but `allow`, with libpq's
rules but for the default: `verify-full` for a host off this machine,
`prefer` only for `localhost` and a loopback address
(`client::on_this_machine`), a mode the string or `PGSSLMODE` names
always winning. libpq's `prefer` everywhere -- v0.2.0's, the first release
with TLS (v0.1.x spoke plain text alone) -- spoke to whoever answered a
connection string copied across a network and checked nothing; the
default is now the one that keeps a stand-in out, a deliberate break
([compatibility.md](compatibility.md#changed-on-purpose-before-10)),
said once at each start (`Url::tls_said`: the shell's `tls` line, the
server's `follow:` log line), and a refusal under it names the cause,
the CAs trusted -- `sslrootcert`'s file, libpq's root file or Mozilla's --
and the ways out, a CA to trust or `sslmode=require`. The rules: `prefer` goes on in plain text on an
`N` and dials again without TLS when the handshake fails (fe-connect.c's
`CONNECTION_FAILED` after it: failing there would keep out no one, since
whoever can break a handshake can as well answer `N`), never after an
error; a CA file makes `require` a `verify-ca` -- `sslrootcert`'s, else
libpq's own `~/.postgresql/root.crt` (`%APPDATA%\postgresql\root.crt` on
Windows), looked for at each dial; `verify-ca` takes a file, never the
public roots, since with every public CA trusted and no name checked any
certificate a public CA signed passes; `sslrootcert=system` (the roots
compiled in, webpki-roots) is `verify-full`'s alone. With no file named
and none at libpq's place, `verify-full` trusts the public roots where
libpq refuses: a static binary in a scratch image has no home and no
store. `prefer` reads no file, where libpq's checks a chain against one
only to go on in plain text if it fails. The answer to the request is one
byte read alone off the socket, so bytes someone put after an `S` go into
the handshake, which refuses them, never into what is read as the
server's (CVE-2021-23222, libpq's). An error in answer is said to be one,
its text never read: it comes before TLS and any password, so anyone in
the path writes it, and shown it would put their words in a log or a
terminal as the server's (libpq stopped showing it in 17.2,
CVE-2024-10977). The same holds after the handshake wherever the server
is not proven (`Client::proven`: this machine, a chain checked against a
CA, or a sign-in bound to the session): an `ErrorResponse`'s own words
are not shown, its SQLSTATE -- held to five digits or capitals -- and this
client's words for it are (`server_error`, `sqlstate_words`). What is
shown of a server's -- an error's words on a proven connection, a column's
name, a cell that does not parse, `server_version` -- goes through
`peer_text`: control and bidirectional characters escaped, cut at 512
bytes (a cell at 64) with the length said; the follower's log lines and
the shell's errors pass through it again, cut at 4 KiB, so a server's
escape sequence never reaches a terminal or a log as one. Every release
since v0.1.0 printed them raw. A build
without the `tls` feature takes `prefer` as plain text and refuses the
rest, as libpq built without SSL does. The wait for the next replication
message is `TlsStream::readable` over TLS: it takes whole records in under
the wait, where a peek at the socket woke for a record holding no
plaintext -- a TLS 1.3 session ticket, a key update -- and left the read
to wait out the socket's 30 s timeout. `make import-test` runs the live
tests with TLS on in the server (`make pgvector-tls`, fennec-wire's test
CA), so every one of them, `--follow`'s included, goes over TLS under
`prefer`, and one under `verify-full`.

SCRAM's iteration count is the server's to choose, and so, under any
`sslmode` short of `verify-full`, whoever stands in for it, and PBKDF2
runs it before the proof goes out: `i=4294967295` was 54 trillion
instructions of PBKDF2 at `fennec-server`'s opt-level 3 and 84 trillion at
the `fennec` binary's z -- more than half an hour of an M1 core even at
its widest, eight a cycle at 3.2 GHz, and some three hours by the CPU
time 2^20 rounds took on this machine shared with other work (2.9 s; 1.2
s keyed, below) -- `fennec import`, or `--follow` connecting again,
waiting on it. The client runs `SCRAM_MIN_ITERATIONS`, 4 096 (RFC 7677's
floor, PostgreSQL's default; below it a proof is cheap to guess from, and
`i=1`, which every release took, makes it four HMACs), to `SCRAM_MAX_ITERATIONS` (2^22, 1 024
times PostgreSQL's `scram_iterations` default of 4 096: 28.6 thousand
million instructions at the bound, 51.5 at opt-level z, which took 6.3 s
and 9.2 s of a core here under a load average past 40) and refuses any
other count as the server-first message is read, however many digits it
has, naming the range and what brings a server under it: the role's
password set again, its verifier made under a lower `scram_iterations` --
`scram_refuses_iterations_past_the_bound` through a stand-in, no proof
sent, and `scram_iterations_are_held_to_their_range` at its edges. A
server set past it -- `scram_iterations`, from PostgreSQL 16, takes 1 to
2^31 - 1 -- is refused by name, where libpq takes any count from 1
(`fe-auth-scram.c`). The bound was 2^20 at first (7.2 thousand million
instructions, 12.9 at z): a hardened setting near a million -- OWASP asks
600 000 rounds of PBKDF2-HMAC-SHA256 -- had barely room under it, and four
times it is still seconds of a core where the most a hostile count can ask
is hours. PBKDF2 hashes the password's HMAC
blocks once (`HmacKey`), each round two compressions where an HMAC made
afresh took four: 6 780 instructions a round where it was 12 641 at
`fennec-server`'s opt-level 3, and 12 250 where it was 19 529 at the
`fennec` binary's opt-level z (`profile.cli`'s for fennec-http) --
instructions retired over 2^20 rounds, the module's code built alone, a
count no load moves.
The exchange's messages are `Scram`'s, the nonce given, so
`scram_rfc7677_exchange` holds them to RFC 7677's example byte for byte,
the proof and the server signature among them; the server signature is
compared by `ct_eq`, as every signature is
([conventions.md](conventions.md#secrets-compared)). The server-first
message is held to RFC 5802: its nonce must begin with the client's and be
longer, and an `m=` extension is refused; the client is driven to verify
the signature by `the_client_refuses_a_server_signature_it_cannot_verify`,
a stand-in sending a wrong one and one sending `AuthenticationOk` in its
place. A password that is not ASCII is PBKDF2's as its UTF-8 bytes, not
SASLprep'd: libpq sends exactly that where its SASLprep refuses a
password, and the verifier a server made of one SASLprep leaves as it is
-- one in NFKC, as one typed nearly always is -- is the same; a password
SASLprep would change (a compatibility character, a non-ASCII space) fails
here where libpq signs in, a gap the docs name and a table of NFKC's
would close.

*Who may ask for the password.* Holding one, the client signs in by
SCRAM-SHA-256 alone (`RequireAuth::default_for`), libpq's
`require_auth=scram-sha-256`: a request for it in clear text (`R3`) or as
MD5 (`R5`) is refused before anything goes out, as is an
`AuthenticationOk` before SCRAM's exchange finished -- a stand-in needs no
password to send one, and taking it proved nothing. Every release since
v0.1.0 answered `R3` with the password under any `sslmode`, so whoever
stood in for the server -- anyone on the path, under `prefer` -- had it.
`require_auth` is libpq's, its rules (`check_expected_areq`): a list of
`password`, `md5`, `gss`, `sspi`, `scram-sha-256` and `none`, all negated
or none, `none` letting a server ask for nothing; MD5 is never spoken.
Without a password a server asking none is taken, as before.

*The sign-in is bound to TLS where the server offers it.* Over TLS the
client takes `SCRAM-SHA-256-PLUS` with `tls-server-end-point` (RFC 5929):
the channel binding is the hash of the certificate it was shown, by the
certificate's own signature hash -- SHA-256 for MD5 and SHA-1, RSA-PSS's by
its parameters, none for Ed25519, refused -- computed in fennec-tls with
rustls's own ring (`server_end_point`, the DER walk bounds-checked and
held by `a_certificate_cut_or_changed_never_panics_the_binding`, and
fuzzed: `crates/fennec-fuzz`'s `certificate` target). A relay under
`require` or `prefer` that passes the offer on shows its own certificate,
so the server's comparison fails and the sign-in with it. One that takes
`-PLUS` out of the offer and dials the server in plain text is not caught:
the client, offered no `-PLUS` over TLS, signs in unbound saying `y,,`
(RFC 5802: it could have bound), and PostgreSQL refuses `y` only on a
connection of its own over TLS (`ssl_in_use`, `read_client_first_message`)
-- so a server whose `pg_hba.conf` takes the role by a `host` line, not
`hostssl` alone, signs the relay in, which then reads the rows and can
write the stream's commits. What keeps that relay out: `verify-full`, where
no certificate but the server's passes (the default off this machine), or
`channel_binding=require` at the client, which refuses a server offering
no `-PLUS` and a connection in plain text; under `require`, `hostssl` alone
at the server too. Under `prefer` `hostssl` does not: the stand-in answers
the client's `SSLRequest` with `N`, takes its sign-in in plain text --
`n,,`, nothing to bind -- and relays it over TLS of its own to the server,
which takes `n,,`, its offer of `-PLUS` never shown to the client. A
stream signed in unbound is not proven, so its keepalives move nothing it
confirms (above). The first form of this section, SECURITY.md and the
import page said a stand-in under `require` got no sign-in it could relay;
the review of the change found the plain-text relay by reading
PostgreSQL's SCRAM, and the three now say what holds. `channel_binding` is libpq's: `prefer` binds where
offered and says `y,,` where no `-PLUS` came over TLS -- a server that did
offer it, its offer taken out on a connection that reaches it over TLS,
refuses -- `require` refuses a server not offering it
(`a_plus_offer_taken_out_is_told_the_client_could_bind`), a connection in
plain text (`channel_binding_required_refuses_a_sign_in_in_plain_text`)
and a sign-in it did not bind, an `AuthenticationOk` with no SCRAM before
it (`channel_binding_required_refuses_a_sign_in_it_did_not_bind`);
`disable` sends `n,,`. A bound sign-in makes the connection proven.
PostgreSQL offers `-PLUS` over TLS from 11 on, so `make import-test`'s
server, TLS on, signs every live test in bound.

**Run on CI.** Those tests -- fennec-import's `tests/pg.rs` (9) and
`tests/follow.rs` (4), fennec-server's `tests/follow.rs` (4), each
`#[ignore]`d for the server it needs -- ran only where a developer had
Docker, so no CI run held this section's invariant until ci.yml's
`postgres (import and --follow)` job, which runs `make import-test`'s two
cargo lines on every pull request, merge-queue run and push to main. Its
server is a service container given what `make pgvector-up` gives its
own: the Makefile's `PG_IMAGE`, PostgreSQL 17.12 with pgvector 0.8.7 by the
digest of the tag's index (amd64 and arm64), its options and its port,
`-c wal_level=logical` through the service's `command` (the runner's since
2.333.0); `make pgvector-tls` turns TLS on in it once the checkout holds
the certificates. The job fails unless cargo's two summaries count every
test the three files mark `#[ignore]` passed and none ignored: a filter
that matches nothing once a module is renamed, or a test a feature
compiles out (`verify_full_speaks_tls_to_a_live_server` is `tls`'s),
passes cargo. Either server is ready once `pg_isready` answers over TCP
inside its container -- the service's health check, `pgvector-up`'s loop,
each 120 tries a second apart at most: the image's entrypoint first runs a
server on the socket alone for its setup and stops it, a second of a 9 s
start, and a check over the socket could take that one for the real server
and hand the next statement one shutting down. `make pgvector-tls` ends
once a session over TCP is encrypted, `pg_reload_conf()` being a signal
the server takes in its own time. Nineteen runs of both lines on an Apple
silicon Mac beside other builds (load average 22 to 76) all passed: the 13
in 15 to 103 s, the 4 in 5 to 22 s, every `converge` within 4.3 s of its
20 (six runs timed); the long one is `follow::a_broken_stream_loses_nothing`,
66 connections each under TLS and SCRAM in a debug build.
