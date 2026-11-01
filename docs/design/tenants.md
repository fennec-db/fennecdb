# Tenants and leases

The long form of the invariants [CLAUDE.md](../../CLAUDE.md) lists under *Tenants and leases*: scaling out by tenant, the router's directory, moves, failovers and leases. Each section below is a paragraph moved word for word from CLAUDE.md, which keeps its rule in a few sentences and links here; "above" and "below" in them refer to the order the invariants had there, which [README.md](README.md#the-original-order) lists.

## Scaling out by tenant

**Scaling out is by tenant, one file each** (`fennec-server --dir`, `fennec-shard`,
whose directory replicates to a standby router like any other file, the
standby's maps catching up from the change ring -- 4 us a change at 100 000
tenants against 38 ms reading them all again under the router's write lock,
as the primary's writes land, and ending what the standby had out to a node
a failover took a tenant off ([router.md](router.md#failover-cuts)) --
reading `failed_from` beside each placement took a change that moves a
tenant to 4.1 us and one a failover made to 4.3, from 3.9;
`site/content/docs/sharding-design.html`). The tenant comes from the path
(`/t/<tenant>/`, looked up again per request, so a move, an idle close or
a delete between two of them is seen), never from the query, so tenants
cannot share a file -- they
would read each other's rows. A file each also keeps ids, the change sequence,
BM25 statistics and `lookup` per tenant, which is why the router forwards bytes
and never parses a query. The registry (`fennec-http/src/tenants.rs`) opens a
file only under its lock after checking the open map, and closes one only when
the map holds the last `Arc` -- a second `Database` over one file corrupts it
exactly as a second process would. `freeze` takes a per-tenant gate
exclusively so a write that passed the frozen check cannot land after the final
export. A move is freeze, copy the image, install, flip the directory in one
statement, delete the source; the change sequence travels in the image, so a
caught-up subscriber resumes on the target without a reseed. A node's tenants
are replicated to its standby, each through its own feed at
`/t/<tenant>/_replication`, and `POST /_shard/nodes/<n>/failover` promotes
them there one at a time -- separate databases, nothing to make atomic
between them. A tenant's role is its file's, not the node's `--replica-of`:
a replica's file follows, a primary's stays one, and a file new to a replica
node follows (the standby copy the router made). Every file on a replica
node was made to follow once, so an idle close or a restart undid a
promotion and the old primary's image wiped the writes since. A rejoining
node's tenants follow when the router records the pair
(`POST /_admin/tenants/<t>/follow`, each), a failover that moved every
tenant ends the pair, and no tenant is placed on or moved to a standby. A
tenant whose follower runs is never closed as idle: the follower holds the
database rather than the tenant, and a close left it writing the file under
the next instance. Nor is one whose database anything else holds beside the
queries -- a compact or a warm-up for its whole run (`Tenant::db_in_use`):
the database holds the file's `<file>.lock`, let go only as it drops, and a
close meanwhile had every request's open wait 5 s for it and answer 500
(`docs/design/engine.md`, single writer). The router cannot tell a node that is gone from one it cannot reach,
and guessing makes two primaries, so it promotes on its own only under a
lease (`fennec-shard --auto-failover`, nodes `fennec-server --dir --lease`;
`fennec-shard/src/lease.rs`, `fennec-http/src/lease.rs`): a node stops
writing as its lease lapses by its own clock -- measured from a moment the
router acknowledged, where the router measures from when that answer came
back, and waits a tenth more -- and only then are its tenants promoted.
Each grant is numbered (`grant`, per node from a random start a router
process, `Leases::number`) and names the newest grant whose answer came
back within the pool's third of a lease (`acked`); the node keeps when it
answered each of its last 16 (`State::answered`, cleared as the holder
changes) and runs its lease from the one acknowledged, never from a
grant's reading (`fennec_http::lease::Numbered`, `grant_from`). Run from the
reading, a grant the router gave up on (`Answer::Silent`) and the node read
late -- held on its way, or left unread by a node stopped or slow -- was a
whole lease after the router had failed the node over, both copies
answering writes 200 (the review of #340's `late_grant.rs`), however late
it came. Read late now, a grant acknowledges what the router had counted
by its sending, a lease the failover outlasted; an answer that came back
late is acknowledged by no grant; a number the node never answered, or
answered for another holder, runs nothing; and a router's restart numbers
afresh, so a grant of the one before, read late, names no number this one
acknowledges. A grant is newer than the one the lease came from
(`State::basis`) by the answer it acknowledges, then by its own number --
one router process's, counted on as it sent them, the only grants that
name one answer -- and taken whole where it is newer, its list with the
lease `ms` from that answer, which may cut the lease held short; its list
alone where the lease has lapsed; not at all otherwise. The list replaced
on its own, as the design first had it, a late grant's list from before a
failover named the tenant again beside the lease a newer grant ran
(`late_grants.rs`'s `a_grant_read_late_takes_no_list_beside_a_newer_lease`);
judged by the lease it would run, as this branch first had it, a router
restarted or promoted with a shorter `--auto-failover` never cut a longer
lease short, failed the node over by its own and left it writing beside
the new primary, and a grant sent before another acknowledging the same
answer, read after it, put its older list back (the review of #343,
`a_router_restarted_with_a_shorter_lease_...`,
`a_late_grant_acknowledging_the_same_answer_takes_nothing`). The node says
whether it took a grant (`"took"`, which a node from before never says and
which means taken), and one it answered and did not take -- the first of a
router restarted or promoted, whose predecessor's lease runs -- is
`Answer::Kept`: the grant acknowledging it goes at once, with the list, and
is newer. Counted taken, the router believed the node held its list, and a
create right after a restart had its schema refused (503) and was taken
back. Two grants make a lease besides, so a grant whose answer says the
lease is short -- none, or under half a lease, what a node new, restarted
or back from a failover holds -- is followed at once by the one
acknowledging it too (`Leases::short`): left to the next round, a tenant
created with its schema was refused it (503), and so was the first write
after every create in the lease tests. A router from before numbers sends
none and its grant runs from the reading, as before, and a node from
before them ignores them (`Leases::unnumbered`, said once): leased the old
way rather than refused or failed over never, so a rolling upgrade in
either order keeps every node writing and failed over on its own, the late
grant open where either side is old. The node's lease runs on the boot
clock (`lease::Moment`: `CLOCK_BOOTTIME` on Linux, `mach_continuous_time` on
Apple's systems through `clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW)`,
declared by hand), which counts a suspend: on `Instant`'s
(`CLOCK_MONOTONIC`, `CLOCK_UPTIME_RAW`) a node suspended past its lease
woke with the lease left as it was and wrote beside its tenants' new
primaries. It fails closed, as std's `Instant::now` does: a boot clock that
stops answering panics, Linux decides once whether it has one (before
2.6.39, `Instant` throughout) -- a failure read past from call to call mixed
two clocks' moments, and a timebase guessed at 1 / 1 made an Apple silicon
lease 41.7 times as long. The router counts on `Instant`, which a suspend
of its own machine stops: it fails over later, never sooner. It cost a
write nothing: a fenced put 555-560 -> 548-552 ns on an M1, about 7 ns
saved, eight runs a side in ABBA turns (`make fence-bench`), and in
Docker's Linux VM (musl), where `CLOCK_BOOTTIME` is read through the vDSO as
`CLOCK_MONOTONIC` is, 1 488-1 677 against 1 472-1 720, quartet by quartet
the same at loads of 34-46 from other work; the clock as it stands, read
again at loads of 50-88, lower in every M1 quartet (804-880 against
821-1 114) and the same on Linux. No runner suspends, so the
clock's test reads it between two reads of a boot clock the system vouches
for -- `/proc/uptime`, `mach_continuous_time` -- which CI runs in a time
namespace whose boot clock is a day ahead of its monotonic one (`unshare
--time --boottime`, the test named whole and its one pass asked for):
reading `CLOCK_MONOTONIC` it fails there, and reading `CLOCK_UPTIME_RAW` on
a Mac that has slept. And a write landed under the lease is not answered as
a success once the lease has lapsed by the time it is answered
(`answered_under_lease`, from `handle_admitted`, the admin schema's route
and a `/_changes` consumer's commit): a process stopped between the fence
and the append, a suspend, or a durability that outlasted the lease -- a
cut stream waits out `SENT_WAIT`, a second, where a lease's tenth is half
of one at the default -- had it answered 200 and lost in the failover. It
is answered 500 with `"outcome": "unknown"`, and `seq` and `history` in the
body rather than in `Fennec-Seq`, which a client would take as its own (Go's
and .NET's code `outcome_unknown`, JavaScript's and Python's `outcome`); a
keyed one is told to send it again with its key, which the lapsed node
refuses before the key is looked up and the tenant settled replays or
makes once, and one with none that a read-back means something only a
lease and a tenth after the answer, the node answering reads with the
write in them until its tenant settles. What wrote is a statement that
writes (`metrics::writing`, a keyed replay's too) or a block landed in the
request (`metrics::landed`, set by `Hub::landed` on the committing thread):
a `/batch` holding a compact, a put landed and a later statement fenced,
was told 503 to come back as if nothing had. Both flags are cleared as each
request begins (`metrics::begin_request`): cleared by `record` alone, they
outlived the admin schema's answer, which records none, and the next read
on its keep-alive connection was answered as a write once its lease
lapsed under it (`tests/lease_answers.rs`, each failing with its part taken
out). What
leases do not cover: a VM stopped by its host is no suspend to the guest --
under QEMU/KVM's defaults a `stop` and `cont` (QEMU sets the kvmclock back,
on arm64 the virtual counter), and under any of its settings a save and
restore or a live migration's blackout, leave every clock in the guest
where it stopped, the boot clock too, as do Hyper-V's saved state, VMware's
suspend, Firecracker's snapshot restore and a container's CRIU restore,
whose time namespace keeps the clocks going on from where they were -- so a
node paused so wakes with lease left and writes beside the new primary
until it runs out or it reads
the router's first grant after the failover. Every signal inside the guest
comes once it runs again and none is sound -- KVM's `PVCLOCK_GUEST_STOPPED`
is the soft-lockup watchdog's, in no interface userspace has; the TSC
against the monotonic clock hangs on the VMM's settings and misses a
migration; the wall clock is set by an agent after, or slewed -- so none is
built; a cloud's notice comes ahead (Azure's Scheduled Events `Freeze`,
GCE's `maintenance-event`), and the RUNBOOK's known gaps say which pauses
count; an answer that passed the second check goes out
whenever the process runs again; a stall of 16 rounds or more can push the
acknowledgement out of `answered`, the node holding no lease until the
grant after, sent at once; two directories leasing one node -- a
misconfiguration -- can take it in turns as each other's lease lapses,
neither holding one for long. A router started with a shorter lease than
the one before it -- restarted, or a standby promoted -- counted a node it
never reached by its own: cut off across the change, the node held the
longer lease and wrote while its tenants were failed over. The directory
records the longest lease a router leasing from it may have granted
(`leasing`, one row, written before a router's first grant where its own
is longer -- `Router::grant_term`, once a process; no block is written at
every start to ride in -- and sent to the standbys before any grant, as a
move's flip is, `SENT_WAIT` at most); a router counts a node by that record
(`Leases::inherited`, read as it leads) until the node has run a lease from
one of its grants (`Held::own`, set by `Leases::ran` on the node's
`"ran":true`, never by `Kept`, never by a take of the list alone -- the
answer read as the object it is, `Granted`: looked for as bytes,
`"ran":true` was sought in windows a byte too long and never found, so
every node was counted by the record until it came down, and a router
restarted at 3 s over a record of 10 s failed a node it had cut off over
11 s after it led, not 3.3 s after the node's last answer --
`late_grants.rs`'
`a_node_that_ran_the_router_s_lease_is_counted_by_it_not_the_record`;
#343's tests held only the other side), and
lowers it to its own once it has led a lease and a tenth of it
(`Leases::lowerable`): every lease granted before it led has run out, and
one started after it counts by what may still run. Never lowered, one 20 s
lease would have every router after it wait 22 s for a node it never
reached. Set by any `Taken`, a grant taken for its list alone -- a
restarted router's first, acknowledging nothing, on a node whose lease had
lapsed -- had the node counted by the router's own lease while a held grant
of the router before, acknowledging an answer the node's ring kept (a
restart keeps the holder), ran the longer one again: two primaries (the
review of #343's probe, `a_take_of_the_list_alone_...`). The node says
`ran` as it says `took`, and a node from before says neither, so it stays
counted by the record. A raise that cannot be written grants no longer than
the record, or this router's own where there is none: refused, every
node's lease lapsed and every node was failed over by a router whose
directory then refused the failovers too. The leasing thread's rounds come
a third of the lease granted apart (`Leases::granting`, the thread woken
as a grant finds it shorter, `grants_short`), and its first at once: a
third of its own apart, a 20 s router over a 1 s record granted a second
every 6.7 s and the node held no lease in 158 of 187 samples, and the
first round a third of its own lease after the start left a node without
a lease from the end of a shorter one before it -- a router whose lease
the record was raised to as well (`late_grants.rs`'
`a_router_that_cannot_record_its_lease_renews_at_the_lease_it_grants`, 84
of 113 samples without on e5b8413e). Each round after the first is due a
third after the last was due, not after the last woke
(`lease::round_due`), and one woken more than a third late counts from its
wake, so at most two rounds come together -- one woken just under a third
late, followed at once by the next, due as it starts -- and never the run
of rounds a stopped process missed: counted from the wake, every wake's
lateness put the rounds after it as late, and at background QoS, where a
park of a second wakes up to 0.2 s late, the rounds of a 3 s lease came
1.18 s apart on average and two of them up to 2.40 s, a node's next grant
left 0.6 s to be late in before its lease lapsed; 1.00 s and 2.04-2.18 s
now (a stand-in node timing the grants it was sent, four runs a side in
ABBA turns, the same beside six busy loops). `lease_stalls.rs`'
`the_leasing_thread_s_rounds_are_due_from_when_the_last_was_due` holds the
leasing thread to it by what it works each round's due out as, not by a
clock (`Router::rounds_on_schedule`). A thread finding the record raised
by another of this router's waits for the standbys too before its grant
(`standbys_sent`, at once once sent): each node's grant is a thread, and
the second node's went while the first waited. What it does not cover: the first
start after upgrading from a router without the record finds none and
counts by its own lease, and a record is a directory's, so a node moved from
the global router's leasing to a region's is counted by the region's alone
(the RUNBOOK says how to lower a lease and move a node). A router from
before the record writes and reads none (`late_grants.rs`'
`..._waits_out_a_node_it_never_reaches`, a restart and a standby promoted
at 3 s after 10 s, each failing with the record ignored or never written,
and `a_router_that_cannot_record_its_lease_grants_no_longer_than_the_record`).
Those tests' shorter lease is 3 s, as the other lease tests' is: at 1 s,
once a node that ran it was counted by it, the healthy nodes beside the one
cut off -- each grant bounded by a third of the lease, 333 ms -- went
unrenewed past 1.1 s at background QoS and were failed over, the replica
their failover dropped leaving the cut-off node's tenant nowhere to go.
Cut off, a node
stops about a round sooner, its lease run from the grant before the last it
read: at a lease of a second it stopped 386-406 ms after the cut against
main's 714-777, its 10 tenants writing again on their replicas 1.27-1.36 s
after against 1.26-1.42, four runs a side in ABBA turns; in a region keeping
its own directory 0.38-0.63 s and 1.41-1.70 against 0.67-0.98 and 1.35-1.73,
ten a side (at 5 s the bench cuts before the first round, so the node's
lease runs from its tenants' creates' grants: 3.9-4.1 s against 3.9-4.3). A
node's first create with a schema, its second grant sent at once, took 8.8
ms either way, eight a side; the router's added p50 stayed 20.5 and 20.6 us
against 20.5 in two sets of sixteen ABBA rounds (per round -0.3 to +0.4,
one +1.3; nothing on the path changed), and `fennec-shard`'s `__text` grew 64
bytes, `fennec-server`'s 2.3 KB. The fence is
the engine's (`Database::set_fence`), asked in `commit` as a block lands and
in `may_write` before a schema change or a maintenance, so every write path
honours it; asked as a lone write started too, it took a `put` from 827 to
898 ns, once 822 to 846. The lease names
the node's primaries, not the node: a node back from a failover writes none
of its stale copies before the repair its answer triggers has them follow.
The list rides only when its epoch (FNV of the sorted names) changed, and a
node that does not hold it answers 412; a create or a move grants the new
list at once. Grants go out each on a thread of its own over a pool
bounded by a third of the lease, and a round waits for none of them
(`Router::round`): it waited for every grant before it looked for a lapse,
and in V5's drill a grant to a node gone, recorded by a name its resolver
was slow to refuse -- the lookup bounded by nothing -- held the round up
until the region's healthy nodes stopped writing (18 s in CI) and were
failed over themselves, the dead node's tenants writing again 30 s after
the kill. To one node a grant goes one at a time, a round sending none
while one of its own is under way (`Leases::begin_grant`; sent anyway,
one that took its whole bound had another queued behind it every round),
and never with a list read before the last one it was sent
(`Leases::turn`, each reading of the directory numbered): a round reads
every list as it starts, and its grant, landing after the one a create
sent at once, put the list from before the create back, the new tenant
refused on its node (503) until the next round. A lapse a round suspects
(`Leases::suspects`) is judged again in the node's turn by a failover on a
thread of its own (`fail_over_lapsed`, `Leases::confirm_lapse`): no grant
to the node that the router still waits on is in flight then -- an answer
that comes back says the node is there, which the leasing thread had
because it ran the failover between rounds; one the router gave up on runs
nothing -- and the turn is held
until the node was sent the list the failover left, so no grant over a
list read before it lands after; it is let go of before the other nodes'
grants are waited for, or two nodes failed over at once waited on each
other's turns for good. A repair runs on a thread of its own, one at a
time (`repair_soon`): on the leasing thread, a node back from a failover
had it call a node gone, 5 s a call for its name's lookup alone, 20 s with
no lease renewed, and the healthy node it ran for failed over at its end;
run in the grant that heard the node back, it held that node's renewals
and had it failed over again. A tenant whose claim another operation held
as its node failed over -- a repair, which runs beside the failovers now,
a quota being set, a move -- is failed over once let go of
(`fail_over_held`, waiting on the claims' condvar), in the node's turn and
only while the node has answered nothing since: left, as a failover by hand
leaves it to be asked again, it stayed on a node gone for good. It goes to
the copy it had as the failover ran and to no other (`HeldTenant`, pinned
then, and only while that copy is still its own): read again once let go
of, the copy was the one the repair holding it had just attached,
following the node gone, and the tenant went onto a file holding none of
it, its rows wiped from its old node once that came back and followed it
(the review's repro). It is out of the node's grants from the failover on
(`Leases::withhold`), which would hand it back to the node as it answered
again, and promoted no sooner than a lease and a tenth past the node's last
answer -- at once, the failover's lapse being that; for a node from before
numbered grants, which runs a lease from any grant it reads, past the
router's last grant to it, answered or given up (`settled`), as before
them, the late-grant hole's window a grant naming it stretched to the
whole wait. The wait ends at `HELD_WAIT` (an hour), the node gone from the
directory or this router demoted -- `promote_on` asks `following()` before
every promotion -- the tenant left where it is and named again; and a pair
ends once its node holds no tenant, promoted, moved or deleted alike. Two
guards besides, older than all of it. A node refuses to promote a copy that
is not its tenant as the router knows it (`tenants::Expected`, the
promote's body: 409, the tenant left where its rows are until its node is
back): of another lineage than the directory's -- a copy that never took
its primary's image is of its own, a new file's or none on a standby, and
so is a file put in its place by hand -- or lacking the history the
tenant's primary forked at its last failover, which the router records from
each promotion's answer (`lineages`' `history`, added as an alter, cleared
where the lineage is succeeded) and not heard from the node it follows
since it was last told to follow it: an old primary's copy a repair made
follow the new primary, promoted before it heard from it, took back every
write since that failover -- its lineage the tenant's and its change past
0, so neither caught it. The hello is the copy's own word, kept in
`.follows` (`heard`, written at the first hello after each `follow`, which
halts the follower before it writes the file again: `mark_heard`,
`heard_since_told`; renamed and removed durably, `fs::sync_dir`, or a power
loss could bring back the file before), since the history alone goes stale: a primary's file
restored in place to before that failover (`DELETE` and `PUT .../file`,
`fennec restore --tenants`) keeps its lineage and lacks the fork, so does
every copy that took it, and every failover after was refused, on a lease
for good (the review's `restore_trap.rs`). Kept across a restart, where
`ready` is not: a node restarted between a follow and its hello still
refuses the copy, one restarted after it still promotes it. A copy never
told to follow -- a standby's, made with its node -- keeps no word, and is
held to the history alone. A tenant whose lineage the directory never learnt, or a
call naming none, is held to the one copy known to hold nothing (no write
and no hello since it opened or since it was told to follow). A file of
another lineage put in its primary's place by hand still leaves every
failover refused until the directory learns it (a read replica's look) or an
operator forces one. Refused at change 0 as this first stood, a
tenant not yet written could not fail over at all, its copy ready and of
its lineage (the review's `empty_refused.rs`). `{"force": true}` on `POST
/_admin/tenants/<t>/promote` or `POST /_shard/nodes/<n>/failover` promotes
whatever the copy holds, an audit event on each (`promote_forced`,
`failover_forced`), and `promoted()` then records the other file's lineage
as the tenant's, the one before among those it was. And a repair gives no
tenant a copy while its primary does not answer its `GET /_admin/tenants`
-- one that ran between a node's death and its lapse attached a copy
following nothing, which the failover promoted and the node's return wiped
the tenant into, on main -- and runs again for those it passed over
(`repair_later`, 2 s doubling to 60, eight times): a repair runs only when
a node failed over answers again or an operator asks, so a primary slow at
the one look left its tenant with no replica until then. A name is
looked up within the call's own bound (`fennec_http::dial`, the
router's connections below). In deploy-kit's drill at a lease of 2 s, the
routers' unknown names sent to a resolver that never answers, us-a's
failover was done 2.7-2.8 s after its kill in three runs against main's
5.3, 5.8 and 15.7 s, the region's other tenants' longest gap 17-19 ms
against 8.0-18.0 s, and no healthy node failed over against one or two in
each of main's runs (us-a's tenants writing again 21-26 s after the kill,
one run failing its repair); with every container's unknown names so, the
follower's lookup of the dead primary hanging too, 2.1-3.3 s in three
against 5.7; by names that resolve, 2.0-2.6 s either way. What the drill's writers
waited past that is the router's `--upstream-timeout` on a request to the
dead node, cut by their own 5 s: at `--upstream-timeout 2` they wrote
again 1.96 s after the kill on both builds -- a wait the failover now ends
itself (what a failover ends, below the router's pool). `make shard-bench`'s failovers
as they were, twelve runs a side in ABBA turns at loads under 3 with all
of the above in, the lineage and history sent and recorded among it: a
lease's last tenant writing again 1.28 s after the cut against 1.29
(1.23-1.57 either way, the round's phase), a region's at a lease of 1 s
1.59 against 1.60 and at 5 s 6.20 against 6.21, a pair's 20 tenants 63.5 ms
either way, one node's 10 over their replicas 52 either way, the repair
after it 241.5 against 238, the router's added p50 20.6 us against 20.4. A
standby router forgets
its leases while it follows and counts from its first round once promoted
(`Leases::leading`): it counted from its last round while it followed, up
to a third of a lease before the promotion, and a grant its primary sent
after that round could still run as it failed the node over. `following`
starts set, so one promoted before a round of its own counts from its first
round too, and a router primary from its start from its first round, a
third of a lease after it at most. A node added since counts from its
addition (`Leases::entry`), which no lease of the directory's can have
named before: counted from the router's start, a node added a lease and a
tenth into the router's life was lapsed before its first grant came back,
and with that grant given up on -- a second at a 3 s lease, at background
QoS beside busy loops -- it was failed over at once, every replica it kept
dropped, as `lease_stalls.rs`' traces showed; the count moves on only with
an answer heard, either way, so it never comes sooner than before. A node without
`--lease` is never failed over on its own. A lease of a second: a node cut
off stopped writing 386-406 ms after (697 when it ran its lease from its
last renewal), its 10 tenants took writes again 1.27 to 1.36 s after. A
write is on the standby 0.089 ms after the primary answered it (p99 0.448),
and 20 tenants failed over in 63.5 to 79 ms, the medians of three rounds
(`make shard-bench`). A standby waits
idle and takes all of a node at once, so with `fennec-shard --replicas` a
tenant on a node in no pair gets a replica of its own instead (the
`replicas` collection): on the node holding the fewest replicas of its node,
then the fewest in all -- the fewest in all alone had every tenant of a node
follow on the same other, ties going by name. The node is told
(`POST /_admin/tenants/<t>/follow {from}`) and keeps the URL in
`<t>.follows` beside the file for its restarts (`resume_following`); a file
made to follow ends what it served first (`release`, as a delete does), or
the old primary's feed held it. A failover of a node in no pair promotes each
tenant on its own replica, `POST /_shard/replicas` gives replicas back -- on
a node holding a copy first, so a returning node's old primaries follow --
and a move re-points the replica or, onto its node, places another. Three
nodes, 10 tenants each: one node's failover took 57 ms, 5 onto each other.

## Replicas tried again

**A replica whose attach failed is tried again, and a file the router left
on a node is taken back by its identity, its node judging, or reported**
(`Router::replicate`, `repair`, `try_again`, `left_on`, `look_at_leftovers`,
`reclaim`, `reclaim_noted`, `clear_target`, `directory::Leftover`,
`fennec_http::instance`, `Tenants::reclaim`). With `fennec-shard --replicas`
a create, a move and a repair each give a tenant its replica on another
node: the file made there (`PUT /_admin/tenants/<t>`) and told to follow
the primary (`POST .../follow`). An attach that failed -- the node refused,
or took the follow past the router's `--upstream-timeout`: a node's rename
of `.follows` took 13.3 s beside eleven other suites, and the deploy kit's
routers wait 10 -- was logged and named in the answer's `unreplicated`, and
nothing tried it again. `repair_later` ran a repair again only for a tenant
whose primary had not answered the repair's look, a repair's own failed
attach went into its `failed`, and a repair runs only when a node failed
over answers again or an operator asks: the tenant had no copy to fail over
to until then, and under `--auto-failover` stayed on its primary as that
died, writing nothing until the node was back. Now an attach that fails, on
a create, a move or a repair, has a repair run again in a while
(`try_again`: 2 s, then doubling to 60, eight in a row, about four
minutes), as a tenant whose primary did not answer had, and so does a
tenant another operation held as the repair came to it; a tenant whose
region has no other node free to keep a replica is not tried again
(`Unplaced::Nowhere`: nothing a while changes gives it one). The try after
places once: the repair asks again under the tenant's claim whether it is
still on that primary, active, in no pair and without a replica -- a
create's own try, a move or a failover may have given it one, or taken it
elsewhere, and two repairs may run at once, an operator's beside the
router's -- and records one replica, as before. And it comes back to what
an earlier try left: where the follow landed after the router gave it up,
a copy follows the primary that no directory names, which the repair's look
sees (`GET /_admin/tenants`) and whose node ranks first among those of the
zone the rule picks (`replica_node`'s `holding`, below the zone). Its `PUT`
finds the file (409), noted as left there by the try that failed, and the
node takes it back, the copy the router left, before the replica is made
anew (`reclaim_noted`, below).

**A file found is followed only where its node finds the identity the
router recorded for it.** The router records the identity each file it
places carries (`files`, keyed `<tenant>/<node>`: the create's, a move's
install, a replica's follow; written unsynced beside the placement's own
synced write, so a crash loses at most a record, and the file is then
reported). An attach finding a file (409) where it recorded one sends a
follow naming it (`"instances"`), and the node follows only where the
file still carries it, judged under its slot as a reclaim is
(`follow_as`'s `named`; 409 naming what it carries otherwise): a node back
from a failover has its old primary follow the tenant's new one, as on
main. A stale router's record never names a live primary: an identity
goes with every change of what a file is -- a follow puts the old one
away, a promotion voids it, an install or a create mints one -- so a
primary a newer router placed carries another or none. Any other file
found goes through a move's judgment (`clear_target`) and is noted for an
operator, never followed over; the repair notes each its look found that
neither record names before it picks (`place_replica`). Only the replica
the directory records there, which a move has follow the tenant's new
node, is followed unjudged, as on main. Followed as found, as this branch
first stood, a router promoted behind its primary -- its directory from
before the primary moved the tenant to n2 and wrote, its source on n1 kept
-- moved the tenant off n1 and gave it its replica on n2, the follow
writing n1's image over the live primary
(`an_attach_keeps_a_live_primary_it_did_not_leave`); reported and never
followed, as the review's first answer had it, an old primary was a copy
lost to every failover, and two nodes stayed without one for good
(`an_old_primary_back_from_a_failover_is_its_tenants_copy_again`). A
promoted primary carries no identity, so after a second failover its file
is reported, not followed: minting one at the promotion is a write on the
failover's path (above). The attach's own follow names the identity its
`PUT` gave, so one landing late is refused too. A node
that did not answer the repair's look is given no replica (`replica_node`'s
`answering`, the repair's alone): it would follow nothing from there, and a
try at it each repair was a call bounded by the upstream timeout, the
tenant without a copy meanwhile; free nodes none of which answered are a
failed try, tried again. The rules a placement keeps are the repair's own,
unchanged: a node of another zone first, never a standby or a node in a
pair, none while the primary does not answer its look, the claim held
across the calls -- so a failover on a lease finding the tenant held pins
the copy it had then (`HeldTenant`), none where it had none -- and the
replica recorded only once it follows.

The tries are one count (`Retries`): a fresh failure is due
`REPAIR_AGAIN_FIRST` after it whatever ran before -- counted from the last
wait, an attach failing after a run of silent tries waited out its minute
-- a failure that comes while a repair runs stays due though that repair
left nothing undone, and eight in a row undone are given up on, said in the
log. Only a tenant without its replica is tried again: a file left on a
node schedules nothing (below), so nothing a while changes waits on one,
and a file fails no repair. As this branch first stood, a failed take-back
of such a file scheduled a repair too, which on a router without
`--replicas` gave every tenant in no pair a replica nobody asked for; with
no take-back the router starts no repair of its own on such a router, and
one an operator asked, or a node back from a failover, runs whole as on
main, its own tries after whole too.

A file the router made on a node, or meant to take off one, that no
placement records is noted in the directory (`leftovers`, keyed
`<tenant>/<node>`, with why, the directory's change it was noted at and
the identities its node gave the file, below; replicated with the rest, so
a standby promoted knows them): a create
taken back -- its placement unrecorded, its quota or its schema refused --
whose `DELETE` the node did not take; a create whose `PUT` went unanswered,
which the node may have made all the same; a move undone after its image
was installed on the target (its quota refused there, the flip unrecorded)
whose target kept the copy, and a move whose source would not go once it
was done; a replica whose attach failed once its `PUT` was answered, made
there (201), or a file it found there that neither `files` nor a row
names (409); a copy a repair's look found that neither names; a replica
detached, or a deleted tenant's replica or standby copy, that would not
come off; and an old primary's file on a node that did not answer the
repair's look, the node a failover took the tenant off (`failed_from`),
where the tenant's replica went elsewhere -- answering, the node would
have kept the replica, its file made to follow where it carries the
identity recorded (`note_old_primary`). The `DELETE`s that took a create back ignored their
failure, so the file stayed, unsaid: a create of the name sent again after
the fence's 503 met the node's 409, and so did every create of it there
after.

**Such a file is taken back by its node where it still carries the
identity the router noted, and reported otherwise.** Every repair lists
each file still noted under `left` (`{tenant, node, why}`, said too where
its node did not answer the look, or kept the file), `fennec_router_files_left`
counts them (no label: a tenant's name is in no label), and
`FennecFilesLeftOnNodes` alerts once the count has held half an hour on the
router keeping the directory; the RUNBOOK says how to look at one and
remove it by hand. A repair's look forgets one it finds gone -- its node
answered the look and lists no such tenant -- or that the directory gives
its tenant now (`records_file`: placed there, its node's standby, its
replica, a read replica), or whose node the directory no longer has
(`look_at_leftovers`). The rows are read before the look, and a row is
forgotten only where it was not noted again since
(`Directory::forget_leftover`'s `noted`): a file noted again after a look
began may be one the look missed. Each other row noted under an identity,
its node answering the look and listing the tenant, is asked of the node
under the tenant's claim -- a create, a move or an attach of it makes and
names files on its nodes meanwhile, and a claim held leaves the file to the
next look -- after the directory is asked again whether it gives the
tenant that file now (`reclaim_noted`). A create of the name on a node
holding such a file is answered 409, as on main, until a repair takes it
back.

**The identity.** A node gives each tenant file 64 random bits
(`fennec_http::instance`, `fresh_id`), kept in `<tenant>.instance` beside it
as `.follows` and `.quota` are: minted as a create (`PUT
/_admin/tenants/<t>`) or an install (`PUT .../file`) makes the file, put
away before a follow writes `.follows` and given anew once the follow is
made, gone with `.follows` as a promotion removes it, and answered as
`"instance"` by the create, the install, the follow and the freeze. A
standby (`--replica-of`) gives its copies none at a create or an install:
they follow by the node's word, with no `.follows`, and no router reads a
standby's answers. A router may propose it
(`Fennec-Instance`) for the file its call makes or changes, and notes the
proposal where the answer does not come: a create's `PUT` and a move's
install unanswered, a follow whose answer was lost. Each row carries the
identities the router's calls in the operation that left the file learned
(`leftovers.instances`, space-separated): the create's for a create taken
back, the install's for a move undone, the freeze's for a move's source
that would not go -- one is minted at the freeze where the file had none,
made before identities or promoted, known to no router where the move is
thawed -- the `PUT`'s for an attach that failed, which its follow proposes
again, and the recorded one with it where the follow was over a file
found. A follow naming identities (`"instances"`) is made only on a file
carrying one, and given its proposal; one naming none -- a router's from
before, a file whose `PUT` named none -- keeps a proposal only on the file
carrying it as the follow begins, else gives none (`follow_as`): one the router gave up on and
noted may land late, after the router took that file back and made the
replica there again, and given its proposal the live replica took the
identity a stale row names -- a router promoted behind the one that took
the file back -- which a reclaim of that row took
(`a_follow_landing_late_gives_a_later_file_no_identity`); a proposal of
its own for the follow, as this branch first had, was such an identity. A
row whose identities the node
refuses -- it holds another file there now -- is noted again with none,
kept for an operator, and an attach or a move never writes over a file
noted with none: its node is no place for the tenant's replica
(`replica_node`'s `kept`) and a move onto it is refused.

**The judgment is the node's** (`POST /_admin/tenants/<t>/reclaim
{instances}`, `Tenants::reclaim`), under the tenant's slot lock, where
every change of a file's identity is made: first as the file stands, so a
live tenant of another identity is answered 409 with the one it carries
without its streams ended; then, the file named, once the requests and
streams holding it let go (`release`) and it is closed, again, before it
goes. 200 is the file gone -- taken, or not there -- and nothing else is:
409 names what the file carries (`null`, none), and a node from before
identities answers 404 at a route it lacks, which keeps the file, the
router sending no `DELETE` in its place. Such a node names no identity in
its answers either, so the router notes its files under none, never asked
of; a proposal it ignored binds nothing, as no file carries it. The move's
409 and an attach's go through the same judgment (`clear_target`,
`attach`): a file of the tenant on a move's target that this router did not
note is no longer deleted unjudged -- it was the tenant's live file where a
router promoted behind its primary moved the tenant onto the node the
primary had moved it to -- and the move is refused, the tenant left on its
source and the file noted for an operator; a move that died before its
flip leaves such a file, which costs its next move onto that node an
operator's hand. Noting the install before it is sent would take that cost
back and bring the loss back with it: a standby router holding the note
and not the flip took the copy the primary had flipped to and written.

**The identity is the file's, not its rows'.** A file put in place while
the node did not hold it -- made again by a binary from before this one,
which deletes the file and leaves the sidecar; restored by `fennec
restore`; put there by hand -- is another file under an old identity, so
the sidecar names the file it was written for, its device, inode and birth
time (`Metadata::created`). **No birth time, no identity:** on a file system
keeping none no file is bound and none carries an identity -- a file left
there is reported, never taken back, said once in the log (`birth`) -- as
the device and inode alone, which this branch first took, let a file put at
the name without its sidecar put away (a hand's `rm` and a restore, a
binary from before) take over the inode and inherit the identity of the
file a router left there. An open notes the file's binding as it takes it, one
`stat` (`Tenant::bound`, moved by a stamp while open); the open file
carries the sidecar's identity only where the sidecar names that binding,
and a close -- `close_with`, `shutdown_with`, a reclaim's own -- forgets an
identity naming another file and binds the one naming this file to what
this process's own renames made of it (a `compact`, a checkpoint, a
follower taking its primary's image). Lost to a crash before the close, the
binding names the file before, which the file no longer is: a file kept,
never one taken. Reading the sidecar at each open to check it there cost
15-20 us of an empty tenant's 67. The sidecar is written in place and its
bytes never synced -- cut short, the line does not read, which is no
identity, and readers and writers meet under the slot alone; written beside
and renamed, a create cost 150 us more in process. It names the file's role
as well, `f` where `.follows` was there as it was written and `n` where
not, and an identity holds only in its role. A promotion removes
`.follows`, synced, so the follow's identity goes with it and the
promotion writes nothing of its own: putting the identity away by an
unlink before it took a pair's failover from 4.0 ms a tenant to 5.2
(`overhead`'s pair alone, eight runs a side in ABBA turns at a load of
1.5), synced on its own to 8.3-9.5, and a new one stamped after it added
more again -- one no router learns before a freeze, which mints one where
a file has none. A follow puts the identity away before `.follows` is
written, its directory sync covering the unlink: kept, the create's, in
role `n`, would hold again once a promotion removed `.follows` wherever a
power loss lost the follow's stamp. A create and an install unlink one a
binary from before left where the file is to be, which a file system that
keeps no birth time could give the new file's inode. A standby's copy made
with its node has no `.follows`, so a promotion would leave what it carried
standing: it is given none. Its first field is its layout, `1`, which a
later one raises and this one reads as no identity. Rows written there, or
into the file, by hand under the file's identity are taken with it: the
identity says which file the router left, not what was written to it
since.

Taking them back was this branch's first design, and two reviews found it
lost tenants. Judged by the router -- the names a node listed, then a
`DELETE` -- a standby router promoted behind its primary took back a
tenant the primary had since made on the node and written. Judged by the
node -- `POST /_admin/tenants/<t>/take-back {lineage, seq}`, deleting only
a file that follows a node or stands at the lineage and change the router
noted, under the slot's lock -- two losses remained, both reproduced. A
move undone, its `DELETE` refused, noted the target's copy at the image's
lineage and change; the tenant was later moved back onto that node and its
copy elsewhere deleted, so the node held the tenant's one file at exactly
that place, and the promoted standby's repair took it back, the tenant on
neither node. And on nodes without `--replication-token` every lineage is
none, so a tenant deleted and made again there with one row, at change 2,
was taken back against a note of change 7. The node judged before the
holders of an open tenant drained, too, so a write landing at change 2 was
deleted after it was answered, and a take-back the router gave up on at
`reader_timeout` could land after its next attach or create. Neither the
router nor the node can tell the file it left from a later one at the same
place: a lineage is the history's, a change the history's count, and both
recur. Making the take-back safe needed a per-file identity, roadmap row
47, built as above. Both losses hold now: the move back onto the node has
the node take back the copy the move undone left -- the file the router
left, its identity unchanged -- and installs the image as a file of
another, so the promoted standby's reclaim names an identity no file
carries; and the tenant made again carries its own, lineage or none. The
node judges with the holders gone and under the slot; a take-back given up
on and landing after the router's next create or attach meets the identity
that one gave. Two removals predated the branch and removed what a row may
name: a move whose target answered 409 deleted the file there, and a repair
placing a replica on a node holding a reported copy made it follow, which
over a file of another lineage took the primary's image. Both now go
through the node's judgment, as above.

Four refinements of row 47's design, each where the code or a measurement
showed a hole: the router proposes identities, since a create's `PUT` that
went unanswered left a file whose identity no router learned; the follow
puts the old identity away before the change and gives a new one after it,
since a follow that failed half way, renewed first, left an operator's
file under the identity of a follow that never took it -- which a reclaim
would have taken, rows and all; the identity is bound to its file, since a
binary from before deletes a file and leaves its sidecar, and a file made
again under it by that binary carried the identity the router noted; and a
promotion renews nothing, the identity bound to the file's role instead,
since renewing it cost a pair's failover a third of its time.

`fennec_router_tenants_unreplicated{region}`, with `--replicas`, counts the
tenants with no copy a failover could promote -- no replica of their own and
no standby on their node (`Directory::uncopied`, one pass at the scrape, as
`same_zone_copies` is), every region a node names at 0 or not, no tenant
in a label, the unnamed region unlabelled as `same_zone_copies` sends it --
and `FennecTenantsWithoutReplica` reads it, its summary naming the region
where there is one: the rule was the difference of `fennec_router_tenants`
and `fennec_router_replicas` by router, which counted a paired node's
tenants, whose copy is the standby's, as missing theirs. A router without
`--replicas` sends none, its tenants in no pair kept with no copy by design.

`tests/replica_retries.rs`, a binary of its own (in `all` its proxies'
threads took the binary past macOS's threads a process, and other tests'
spawns failed -- as `tests/read_replicas.rs`, apart since #372, would have
again: its tests held up to 1 195 threads at once and the rest of `all`
868, together 2 028 to 2 045 of the 2 048, past them once the repairs its
routers' failed attaches run again went on beside), puts a proxy before a
node that refuses a request (503), holds it, or sends it on and swallows
the answer, the connection ended with nothing said, and holds, each failing
with its part taken out (in parentheses, what it met then): a create whose
follow was refused once given its replica by the repair after, the follow
asked twice in all and the note of the copy forgotten as it is recorded
(the create's try not tried again: never; the look not forgetting a placed
copy: the count stays 1); a repair whose `PUT` was refused, leaving no file
to wait on, tried again (its own failed attach not tried again: its 502 and
nothing after); a follow that landed after the router gave it up adopted --
the copy's node and another of its zone by then, first by name, which a try
blind to the copy picks -- with no second copy (the copy's node not
preferred: one on the other); two repairs at once, the first held at one
tenant's follow while the second gives the next its replica, giving that
one no second (no look again under the claim: a second on the node with
fewer of the primary's replicas, the first left following unrecorded); a
node silent at the repair's look given no replica though first by name (the
silent one given it); and the gauge counting the tenant and clearing as the
try lands (none sent: `datadog.rs`' rules fail on it too). Of the files
left, each noted, reported with its why by the repair after and still on
its node (unnoted: no word of it, the count 0; a take-back put back in the
look: the file gone): a create taken back whose `DELETE` was refused, a
create of the name there then answered 409 and no `DELETE` sent since, and
once removed by hand the look forgetting it and the name made there (the
look not forgetting a file gone: the count stays); a create whose `PUT`
went unanswered; a deleted tenant's replica that would not come off; a move
undone whose target kept the copy; a move's source that would not go; a
copy a failed attach made where the replica then went to a node of a better
zone; a replica detached that stayed; a file put there by hand that an
attach found (409) and failed on, its rows in place; an old primary's file,
its node silent as its tenant's replica went elsewhere (unnoted: no word of
it); and a file on a node silent at the look, removed by hand meanwhile,
reported with its node's silence and forgotten once the node answers (a
silent node read as one without the file: forgotten unseen). The review's
two losses each end with the tenant whole: a tenant moved back onto the
node a move undone had left its copy on, its copy elsewhere deleted, and a
standby router promoted with its directory from before the move back,
whose repair reports the file and leaves it (a take-back: the tenant on
neither node), the primary's own look forgetting the note as the tenant is
placed there; and on nodes without `--replication-token`, a move's source
noted, the tenant deleted and made again there by hand with one row,
reported and kept (a take-back: the row gone). A router without
`--replicas` and one of pairs alone schedule no repair for a file left
(`Router::repair_due`, asked at once; a note scheduling one: a replica
given 2 s after), and place no replica. The lib's tests hold the one count
of tries: a fresh failure due 2 s after it, though the tries before waited
out their minute, one that came while a repair ran still due, eight undone
given up on. `directory::tests` hold the count by region and a pair's
standby as a copy, and the files left noted once by tenant and node, noted
again at a later change, forgotten by a look only where not noted again
since it read them (the change ignored: the later note forgotten), kept
across an open, and made where a directory lacks the collection.

Row 47 turned five of those around: a create taken back, a create whose
`PUT` went unanswered (the proposal the node took), a move undone, a move's
source (the freeze's identity) and a copy a failed attach made (the
`PUT`'s, which the follow that landed kept) are each taken back by the repair after, `left`
empty and the count 0, the create's file by its node's judgment and no
second `DELETE`, the name made there after. Still reported and kept: a
deleted tenant's replica and standby copy, a detached replica, an old
primary, a file on a silent node -- none named by an identity -- and the
file put by hand an attach found, which carries its own. The review's two
losses run as they did, the tenant whole at the end, and now prove the
judgment: the move back has n2 take back the copy the move undone left
(one reclaim, one `DELETE` in all), the standby's repair asks n2 and is
refused -- reported with its node holding another file there now, and asked
no more -- and on plain nodes the repair's reclaim is refused by the tenant
made again; with the node's check taken out both lose the tenant again, the
attach's found file its rows. New: `a_file_of_no_lineage_left_is_taken_back`
(plain nodes); `a_follow_and_a_promotion_change_a_files_identity` (with
the follow's renewal taken out, the same identity before and after; with
the role's check taken out, the promoted file kept the follow's; closed and
opened again, none, and a freeze mints one);
`a_node_from_before_identities_has_no_file_taken_back`, the proxy dropping
every `instance` and `Fennec-Instance` and answering `/reclaim` 404: a
create taken back noted with none and asked of nobody, and a create whose
`PUT` went unanswered asked, refused 404, and kept with no `DELETE`. Its
review's: `an_attach_keeps_a_live_primary_it_did_not_leave` (the attach's
judgment taken out: n2's live primary followed, its writes refused and
taken over); `a_follow_landing_late_gives_a_later_file_no_identity` (the
follow's check taken out: the replica made again took the first follow's
identity, which a reclaim of it took);
`an_old_primary_back_from_a_failover_is_its_tenants_copy_again` (two
nodes: the old primary followed by the identity its create gave, a follow
naming another refused 409 and the file unchanged; noted instead, 502 and
no copy); the failover tests of `regions.rs`, `replicas.rs`, `leases.rs`,
`lease_stalls.rs` and `read_replicas.rs` as on main; and `instance::tests`, a birth time
missing or 0 binding no file and a sidecar naming none no identity. In
`tests/router.rs` a move onto a file it did not note is refused 409, the
tenant reading on its source and the file counted, and goes once the file
is removed. `fennec-http`'s `tests/tenants.rs` holds the node: a reclaim
naming another identity answered with the one the file carries, the file
and its stream kept; the identity across a close; a reclaim of an open file
ending its stream before the file and its sidecar go; one of a file gone
200, and a body naming none 400; a router's proposal taken; a tenant made
again refused by the identity before; and a file put in place by a rename
while closed carrying none, its sidecar forgotten as it closes. The lib's
`instance::tests` hold the binding: a write in place keeps it, a rename
into the place loses it, a close rebinds what the file's holder renamed,
an identity holds only while `.follows` is there as it was (one given
before a follow goes as it comes, one given with it as a promotion removes
it, a `heard` mark changing nothing), and a layout, a role or a line it
does not read is none. `make upgrade-check`
drives a node from before: a create answering no identity, and a reclaim
of its file, noted by hand in this tree's router's directory, answered 404
and kept.

Nothing of it is on a request's path: the tries run on the repair's
thread, and a create, a move and a delete write a row only where a call
failed.
`make shard-bench ARGS=latency`'s added p50 stayed 20.6 us (20.3-20.9)
against main's 20.75 (20.6-21.8), eight runs a side in ABBA turns at loads
of 1.8-2.05, the quartets' differences -0.65, 0, -0.2 and -0.35 us; a pass
before it at loads of 2.2-2.5, other agents' work beside it, had outliers
on both sides (main 21.8, this 27.7) and medians of 20.75 and 20.7.
`fennec-shard`'s code grew 25.0 KB (`__text`, aarch64-apple-darwin,
`size_report.py --bin`, 2 737.4 -> 2 762.4): the directory's `leftovers`,
`failed_from` and `uncopied` 5.7 and `write_block` out of line now that two
call it 3.5, noting a file 4.2 (`left_on` with the directory's write in it,
`take_back`, `said`), the retries' waiting thread 2.3 net of
`repair_later`'s, the repair's look and the move's notes 5.4 net of where
inlining moved them (`repair` and `relocate` out of line, `admin` and
`promote_on` smaller), the rest the delete's and the attach's notes and
the gauge. `fennec-http`, and so `fennec-server`, is main's to the byte.

Row 47's identities cost no request a thing and a failover nothing
measurable. A pair's failover (`overhead`'s `replicas()` alone, sixteen
runs a side in ABBA turns at loads of 1.9-2.1) took 4.05 ms a tenant
against main's 3.96 (means; medians 4.05 and 4.00), the router's call to
promote 3.86 against 3.76, the spreads of either covering the other's
(3.4-4.6); a failover onto replicas 9.5 against 9.05 (medians, eight runs,
7.8-10.1 against 8.0-9.7), the promotion writing nothing of the
identity's. A create with its replica through the router took 46.0 ms
against 45.5. In process, `Tenants::create` took 330 us against 262
(medians of eight runs, 200 tenants each): the sidecar written, a `stat`
of `.follows` and the unlink of one a binary before left, the one cost a
create pays for the identity a router notes; a reopen 66.3 us against 65.8,
its one `stat`. Code: `fennec-server` grew 19.6 KB (`size_report.py
--bin`, 3 288.2 -> 3 307.8), `fennec_http` 19.4 of it -- the `instance`
module 6.7 (`read`, `write`, `closing`, `parse`), `Tenants::identity` 2.4,
the reclaim's route and judgment and the answers naming an identity the
rest; `fennec-shard` 15.3 (2 936.3 -> 2 951.6): `reclaim_noted` with the
reclaim in it 4.4, `place_replica`'s nodes kept 1.7, `left_on` with its
identities 1.6, the repair's look 1.5, the directory's `leftovers` and
`forget_leftover` 2.2, the move's and the attach's notes the rest.
