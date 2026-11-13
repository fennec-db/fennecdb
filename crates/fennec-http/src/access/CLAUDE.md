# CLAUDE.md

fennec-http's access -- scoped tokens and their rules, field lists, the server's own collections, tenant binding, whom a token is for and its expiry -- for code in `crates/fennec-http/src/access.rs` and `crates/fennec-http/src/access/`, which binds fennec-core's hooks (`plugin::Hook`, `Check`), its `Filter` -- the order it tests an `and`'s terms in -- and its own collections' names (`declared::OWN`) too, and how a replica's sync reads a 401 (`fennec_abi::sync`, `web/fennec.js`); the rest of fennec-http's rules are in [its CLAUDE.md](../../CLAUDE.md), the rules every change meets in the root's [CLAUDE.md](../../../../CLAUDE.md).

## Invariants

### Access

**[A scoped token is held to its rules at every level, twice for
writes.](../../../../docs/design/access.md#scoped-tokens)** A new path that runs a
statement must go through `scoped()` or it reads everything. The rules are
tested before any term of the caller's: ANDed in first wherever they meet
one (`access::and`), so a term that fails never runs over a row they hide,
and an upsert's row is judged before its `else set` reads it
(`Hook::before_upsert`). The order is fennec-core's to keep: it tests an
`and`'s terms that may fail as written, each after every term before it
(`Filter::bind`, `Test::And`, `Chain`, `Filter::without`), only a
term that cannot fail (`Test::rank`) moving ahead of them, and `@ttl`'s
`alive` first (`answer_filter`); a planner ordering terms by cost must keep
it, or a failing term is an oracle again. `explain` is the server
token's (403): a plan counts rows before the rules hold them. A scoped
write outside `within()` goes unchecked, so only a named statement's call runs
one there (`run_as`), and a binding's writes, which the `Check` hook passes
over (`Hook::writer_only`) once `Hook::waking` judged it -- a row passing
its condition or not where that reads a field the token is not shown
(`Scope::reads_unshown`): a `wake` line
grants it (`* wake` refused), its body reaches no `append-only` collection
(403, the audit's `unwoken` line naming it), its runs charged to
`--call-rate` (`calls::charge_runs`), and `judge_call` holds a body to the
same. A write over a row meets `before_overwrite` with the
row as it was, judging `update(a, b)` and transitions (`new.`, granting
`update` alone); `append-only` binds every scoped token. Scoped, a stream
tells deletions only of ids it sent, a poll's tag digests its answer
(`content_tag`) and `match` scores its rows alone (`Match::within`); the
algorithm is the key's, never the token's. A claim held as `null`, `""` or
a value its rule cannot compare (`usable`) is one the token lacks: its rule
does not apply, binding no filter. A claim a rule orders by holds the other
side to its kind under any number of `not`s (`kind_held`), a row with no
value answering as the rule written out; an inner `get`'s rules are bound
for the `not`s it stands under (`hold`). Refused at startup: a claim in a
call or arithmetic (`like`'s pattern aside) or compared in order inside a
value (`claims_held`), and a policy writing its own `$<n>`. A tenant claim
naming no tenant reaches none; a `sub` of `""` is no subject.

**[A field a read rule withholds is held out of what a statement
does.](../../../../docs/design/access.md#field-lists)** `fields::hold` and
`fields::write` guard or refuse (403) each use of it before the rules are
ANDed in; it is `null` in a row (`Scope::shown`), absent from every schema
answer (`Scope::shown_schema`) and refused by name as a missing one, and a
stream sends no row a write left showing the same (`sse::Seen`).

**[A rule may read another collection, answered before each
statement.](../../../../docs/design/access.md#rules-reading-another-collection)**
Its `in` lists are answered under each lock (`Scope::bound`), the scope
`Check`, the mask and streams use; a list past `MAX_CLAIM_VALUES` is
refused, never cut, and rules reading each other's collections, a `*` rule
reading one, or `new.` or `expired()` in one are refused at startup
(`ring`). A stream's filter reading another collection -- a rule's, or the
shape's own inner `get`, any token's -- is answered again at each look
after a write to what it reads, once a row of it reached its `@ttl`, or
a second after the read answering it ended where it reads the time -- the
stream wakes for both (`sse::Reach`), and writes its keep-alive once one
passed with nothing written, whatever woke it -- on a pin where a query of
it would be (`sse::look`); the shape runs again only where it moved, its
rows sent as a diff against those it was sent (`enter_and_leave`).

**[A collection whose name starts with `_` is the server's, out of every
rule's reach.](../../../../docs/design/access.md#the-servers-own-collections)**
`*` reaches every other (`access::covers`, `declared::own`); a rule naming
or reading one is refused at startup, and a scoped token is answered as
for a missing collection on every route -- as for any it is granted
nothing on, whatever a REST request's route read of the schemas its path
and each `lookup` level name (`path_refusal`), a subscription's inner
`get` held to the rules (`Scope::subscribed`). The server's token reads
them, and `scoped()` refuses its statement making, changing, compacting
or writing one (`access::own_refused`), as the schema routes a
description declaring one (`own_described`, off the body, before the
write rate) or a migration still to run reaching one (`own_migrations`,
under the lock; one recorded is not asked). The server
writes its own through the engine, never a statement; a new one is
`_`-named and listed in `declared::OWN`.

**[A token is bound to the tenant it
names.](../../../../docs/design/access.md#tenant-binding)** `route_tenant` asks it,
and no route below `/t/<t>/` may skip it -- a hook's event alone, whose
signature it asks in the token's place, against that tenant's secrets,
before the tenant is looked up; every tenant's database installs the
`Check` hook (`Tenants::check_scoped_writes`).

**[A token is taken only from the issuers, for the audiences and clients,
a server was told.](../../../../docs/design/access.md#whom-a-token-is-for)**
`Demands::whom` asks `iss`, `aud` and `azp` (else `client_id`) in
`Access::check`, after the signature and before a token is kept, so every
route meets it through `authenticate`; a refusal is 401 naming the check,
never the token's value, as its audit line does (`audit::refusal`), and
clears no wait. A server told none takes every token its keys sign (said at
the start under `--jwt-keys`); a `--jwt-*` flag without keys is refused. A policy names top-level claims alone, quoted
where not a word (`claim_name`): a path into a nested object, or an
unquoted name running into `-` or `:`, is refused at startup, never read; a
role is the word after the last `for` or a quoted name (`role_of`).

**[A token past its `exp` is told so by a code, and no other refusal
is.](../../../../docs/design/access.md#expired-tokens)** A request whose
token's `exp` passed is answered 401 `{"error":"the token has
expired","code":"token_expired"}` (`refusal_code`, in `authenticate`), and
a stream ended at the `exp` says the same (`sse::EXPIRED`); the `exp` is
asked after the signature and whom the token is for (`Access::verify`), so
the code is told only of a token this server took until then. A client
reads the code alone, never the words, and no 401 is a sign-out: the
server keeps no list of revoked tokens, so an app asks its issuer at every
one and signs out where the issuer refuses. So a replica's sync hands its
provider the code, whether the refused request carried a token and the
asks in a row since the server took the token held -- a 2xx or a stream
opened, never a 408, 429 or 5xx (`TokenRefusal`, the token action's
`code`, `had_token`, `attempt`) -- asks once while it waits, after 1 s
doubling to 60 s where the token handed over was refused too (an expiry
starting at 1, unless of the token already refused for one), again at
`{"online":true}` where nothing came, and sends a request refused for a
token since replaced again, unasked (`fennec_abi::sync`, `web/fennec.js`,
held by the scenario file); a replica is one account's, its key or path
naming it; every SDK names the code `errorCode`.

**[An empty token is no token.](../../../../docs/design/access.md#empty-tokens)**
A variable set empty is unset (`env_secret`, fennec-shard's `env`); a token
flag, a JWT secret's file or a key file given empty is refused at the
start; a library constructor takes an empty token as none
(`Config::settled`).
