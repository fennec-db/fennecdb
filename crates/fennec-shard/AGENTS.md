# AGENTS.md

The router's invariants -- tenants and leases, regions and the global entry, read replicas and its own -- for code in `crates/fennec-shard/` and the tenant registry it drives in `crates/fennec-http/`. The rules every change meets, and where every other area's are, are in the root's [AGENTS.md](../../AGENTS.md). It is written from CLAUDE.md by `make agents-md`: edit that one.

## Invariants

### Tenants and leases

**[Scaling out is by tenant, one file
each.](../../docs/design/tenants.md#scaling-out-by-tenant)** The tenant comes from
the path, never the query; the router forwards bytes, parsing none; the
registry opens a file under its lock and closes it on its last `Arc` alone,
never while its follower or anything beside it holds the database: no two
`Database`s share a file. A tenant's role is its file's, not the node's
`--replica-of`; a move is `freeze` (an exclusive gate: no write lands after
the export), copy, install, the flip in one statement, the source
deleted once the standbys were sent it (`standbys_sent`). A create or an
install answers once the file and its name in the directory are durable
(`made_durable`, `fs::sync_dir`); either failing is a 500 that takes the
file back under `<file>.lock` (`take_back_made`). A node
is failed over on its own only under a lease, run on the boot clock
(`lease::Moment`) from the grant the router acknowledged, never a grant's
reading (`grant_from`); the router counts a node by the directory's
`leasing` record until the node ran one of its leases (`Leases::inherited`,
`Leases::entry`), granting one at a time, never with a list older than the
last it sent (`Leases::turn`, `lease::round_due`). Every write path meets
the engine's fence (`Database::set_fence`), every route answering a write
asks `answered_under_lease` (500 `outcome: unknown` once lapsed), and a node
promotes only a copy of the tenant's lineage and history
(`tenants::Expected`, 409) unless `force`d.

**[A failed attach is tried again, a file left taken back by
identity.](../../docs/design/tenants.md#replicas-tried-again)** `try_again`
repairs a failed attach under the claim;
`fennec_router_tenants_unreplicated` counts uncopied. A file left is a
`leftovers` row (`fennec_router_files_left`) with its ids
(`fennec_http::instance`); the node deletes it, or follows over it at an attach's 409, only
while it holds the id recorded (`Tenants::reclaim`, `files`), under its
slot; no id, never.

**[A tenant's hook secrets are its file's, relayed by the router and kept
by none of its records.](../../docs/design/named-statements.md#where-the-secret-lives)**
`<t>.hooks` is bound to its file as `.instance` is (`tenant_hooks::Store`,
`fennec_http::instance`): put away, synced, before every create, install,
follow, delete, reclaim and restore (`Tenants::put_away`,
`put_away_beside`), never adopted by another file -- an open puts away one
naming another than it opened, and an event verified against a set no
longer the tenant's is the one 401 (`Store::opened`, `is_current`) --
carried by its open's own renames alone (`rebind`) and kept by a
promotion. `hook-secrets` answers a set to the node's admin token alone,
never logged or traced. The router's `PUT` is refused (409) while the
tenant moves -- its claim, its moving mark (`unmoving`), the frozen source
-- as a quota's is, and is answered once the primary and every copy a
failover could promote hold its version, a copy behind named and counted
(`fennec_router_tenants_hooks_behind`), every one where the set is not read
back, until `repair_hooks`, asking each copy its version, copies it; a
copy that lacks it -- attached, following, a move's target before the flip,
across regions (`Fennec-Hook-Secrets`, router to router) -- is sent it
(`send_hooks`). Its directory holds names and versions, never a secret.

**[A tenant node runs its tenants' schedules from one thread, by name, a
run no use of a tenant.](../../docs/design/named-statements.md#where-a-run-happens-and-once)**
`schedule::Node` keeps each scheduled tenant by its name, never an `Arc`,
and opens it through the registry as its slot comes (`Tenants::for_run`),
closing it after unless a request used it: a run never counts as a use nor
evicts a tenant a client used. `schedules.hint` never misses a tenant
holding a schedule: it is named, synced, before any scheduled definition
or installed file is written (`Node::note`), its look queued only once the
file is in place (`Node::installed`), and it leaves only once its last drop
is durable or as a frozen move's source leaves the node (`Node::left`). One
tenant's look panicking holds that tenant back, never the node. A new way a
tenant opens, closes, comes onto or leaves a node owes the scheduler's hooks
(`opened`, `closed`, `installed`, `forget`).

### Regions and the global entry

**[A node is in a region and a zone, and a tenant in its node's region.](../../docs/design/regions.md#regions-and-zones)**
Its region is its node's, kept nowhere else and fixed while the node holds
a tenant, a replica or a place in a pair (`holds`); a `--replicas` copy
and a pair's standby stay in the primary's region, another zone first
(`Router::replica_node`).

**[A region may keep its own directory, and leases and fails over inside itself.](../../docs/design/regions.md#region-directories)**
A request is relayed once, never again (`regional_admin`), and a move
across regions undoes each step where the next fails (`move_across`). A
node refuses a router its lease's holder superseded (409 `held_by`,
`Lease::admits`), and another directory's grant while its lease runs.

**[One URL reaches every region: the Worker learns homes from the routers.](../../docs/design/regions.md#the-global-entry)**
It serves `/t/<tenant>/`, and `/_shard/` where `ADMIN` is set, never a
tenant's `/_replication`, decoding a path as fennec-http does
(`percentDecode`). It drops every address a client claims and names the
client (`CF-Connecting-IP`) in `Fennec-Client`, an HMAC under `ENTRY_TOKEN` a
router believes only with `--entry-token` and within 60 s (`audit::entry`),
and drops from all it forwards (`forwarded_headers`); a router refuses an
entry token empty or equal to another it holds, at the start and as a node
is added (`set_node`). Every router answer carries `Fennec-Shard: 1`; a request that
may have reached one goes again only as a `GET`, `HEAD`, `OPTIONS` or keyed
-- a tenant's hook event among them (`hooked`) -- or, any request, after a
guessed region's router's 503 marked
`Fennec-Unsent`, which ran nowhere.

### Read replicas

**[Read replicas in other regions answer only marked reads.](../../docs/design/read-replicas.md#marked-reads)**
Only `Fennec-Read: replica` goes near, and
every server runs a marked request read-only on every route (`marked`, read
in `serve_connection` alone); a failover never promotes a read replica. One
is recorded once its node follows, its copy told it is one (`"reader":
true`, `Reader::marked`: its stream no standby); taken off, its record goes
first, sent to the standbys, then its file; no name is made again while a
directory keeps a trace of one (`reader_traces`). A copy answers only once its primary said
hello since it opened (`Tenant::ready`, else 503); a read replica is bound
to its tenant's lineage, which every promotion and restore keeps
(`fork_id`), and deletes itself rather than take another's
(`Follower::expect_lineage`). `Fennec-History` goes beside `Fennec-Seq`, and
`after` waits for that change of that history (`History::continues`). SDKs
send home's furthest position with each marked read, whose answer moves
none; home's 504 sets it, lower or not, only over the position that read was
sent after, never to an earlier term of its lineage. No lock is held across
a call to a node, and a refused hold makes none (`then`, never `then_some`).

### The router

**[The router tells a page one CORS rule, its
own.](../../docs/design/router.md#cors)** With `--http-cors` it answers
preflights and puts its own CORS headers in place of a node's
(`cors::is_cors_header`), never both. A forward carries no `Origin` it
answers for, `--http-cors`'s or its own under this machine's name
(`cors::vouched`), any other left for the node to judge; without
`--token` it is an open server, held to fennec-http's rule, a forward's
`Origin` by its `--http-cors` alone, the node's rule otherwise.

**[The router looks at a pooled connection first, and resends only what
cannot have run or may run twice.](../../docs/design/router.md#pooled-connections)**
A request not sent whole goes again, one sent whole only if `replayable`
(a `GET`, `HEAD`, `OPTIONS`, keyed or marked read, or a `POST` to
`/t/<t>/_hooks/`, keyed by its event: the entry's rule alike), judged by
`Stage`, not errno; head and body are one `writev`. One never sent whole, the retry
spent, is the router's 503 marked `Fennec-Unsent` (`region::unsent`),
never a 502, and never once a try went whole (`Stage::Retried`); a node's
mark is dropped (`answer_out`), a relayed router's too once a hop before
went whole (`relay_send`, `after_cut`'s `ran`): a 502. Every dial goes
through `dial::resolve`, caching nothing; every body read allocates through
`zeroed` and refuses an answer with no length. A held request watches its
client for the head, over TLS through its session (`answered_or_gone`,
`Conn::peer`; `relay_send` at a relay): an end, a `close_notify` too, goes
on to the node, never taken for gone; a reset lets the node's connection
go.

**[A failover ends what the router has out to the node for the tenants it
moved.](../../docs/design/router.md#failover-cuts)** `promote_on` has `Pool::cut`
shut what is out to the node under the tenant's keyed mark (`mark_of`),
one word with the connection's state, never an answer begun (`ANSWERING`)
nor a descriptor reused (`CLOSED` before the close); a cut request follows
`replayable`'s rule, and a forward reads `Pool::moved` before the directory
and before its first byte. Only a failover cuts, granting the nodes it
moved tenants to as it ends (`granted_after`). A router following the
directory cuts too, as a write lands (`Arrived`) naming the node failed
over off (`failed_from`, cleared by every other: a move cuts nothing),
every bucket moved first (`cut_all`); it grants nothing, asking a copy
refusing a replay unrun again while its client stays, for the sooner of two
thirds of the recorded lease and `--upstream-timeout` (`copy_pause`).

**[A query across tenants is the router's admin's, merged as the request
says.](../../docs/design/router.md#queries-across-tenants)** Nodes run it
read-only; the router fails and names a tenant its merge cannot hold
exactly, and refuses (413) past its bounds, never cuts.
