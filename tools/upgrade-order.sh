#!/usr/bin/env bash
# The upgrade's order for named statements (docs/design/named-statements.md,
# slice 4; RELEASING.md): nothing defines a statement until every binary
# reading a tenant's file is upgraded. A binary built before record kind 13
# meets the first `create statement` and refuses it in words, never
# misreading it:
#
#   - a replica following the primary's stream refuses the write ("record
#     kind 13 is not a write") and connects again, never taking one past it;
#   - an archiver keeps the stream's bytes but takes no image of its own
#     ("unknown record kind 13"); the old `fennec verify` and `fennec
#     restore` refuse what it kept, which this tree's read and restore, and
#     the old server refuses to open the file this tree's restore wrote;
#   - a router refuses a tenant's schema declaring a statement as one that
#     does not read (400), placing nothing;
#   - a move onto an old node fails at its install, and the tenant stays on
#     its source, answering, with what it held;
#   - an old node names no identity for a file it makes, and answers this
#     tree's router asking it to take a file back (`/reclaim`) 404, which
#     keeps the file: the router reports it and sends no DELETE in its
#     place (roadmap row 47);
#   - an old router over this tree's nodes, proposing and reading no
#     identity, creates, replicates, moves and deletes a tenant as before,
#     each file given one its node minted, and the delete taking it away.
#
# This builds `fennec-server`, `fennec` and `fennec-shard` at BASE, the last
# main before the kind, runs this tree's binaries beside the old ones,
# defines a statement, and holds each old one to what it said:
#
#   tools/upgrade-order.sh NEW_BIN_DIR [BASE]
#
# NEW_BIN_DIR holds this tree's `fennec-server`, `fennec` and `fennec-shard`
# (`make upgrade-check` builds them). OLD_BIN_DIR, where set, holds the old ones
# already built and skips their build.
#
# Since record checksums (docs/design/storage.md#file-format) a binary from
# before them stops at the first record this tree writes, before any kind
# or op a case below is about: each case runs the binaries of CK_BASE, the
# last main before the checksums (PRE_BIN_DIR, where set, holds them), where
# it ran this tree's, and the last case holds CK_BASE's to this tree's. Every server listens on port 0, found
# by its `listening on:` line; everything it started is killed and every
# path it made removed on its way out.

set -euo pipefail

NEW=${1:?usage: tools/upgrade-order.sh NEW_BIN_DIR [BASE]}
BASE=${2:-12d6f8db^}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=$(mktemp -d "${TMPDIR:-/tmp}/fennec-upgrade.XXXXXX")
PIDS=()

cleanup() {
    for p in "${PIDS[@]:-}"; do
        [ -n "$p" ] && kill "$p" 2>/dev/null || true
    done
    wait 2>/dev/null || true
    for tree in "$WORK/old" "$WORK/old3" "$WORK/old4" "$WORK/old6" "$WORK/pre"; do
        if [ -d "$tree" ]; then
            git -C "$ROOT" worktree remove --force "$tree" 2>/dev/null || true
        fi
    done
    rm -rf "$WORK"
}
trap cleanup EXIT INT TERM

fail() {
    echo "FAIL: $*" >&2
    for f in "$WORK"/*.log; do
        echo "--- $f" >&2
        tail -20 "$f" >&2
    done
    exit 1
}

if [ -n "${OLD_BIN_DIR:-}" ]; then
    OLD=$OLD_BIN_DIR
else
    echo "building fennec-server, fennec and fennec-shard at $BASE ..."
    git -C "$ROOT" worktree add --detach --quiet "$WORK/old" "$BASE"
    CARGO_TARGET_DIR="$WORK/old-target" "${CARGO:-cargo}" build --quiet \
        --manifest-path "$WORK/old/Cargo.toml" -p fennec-server -p fennec-cli -p fennec-shard
    OLD="$WORK/old-target/debug"
fi

CK_BASE=${CK_BASE:-f29a44a7}
if [ -n "${PRE_BIN_DIR:-}" ]; then
    PRE=$PRE_BIN_DIR
else
    echo "building fennec-server, fennec and fennec-shard at $CK_BASE ..."
    git -C "$ROOT" worktree add --detach --quiet "$WORK/pre" "$CK_BASE"
    CARGO_TARGET_DIR="$WORK/pre-target" "${CARGO:-cargo}" build --quiet \
        --manifest-path "$WORK/pre/Cargo.toml" -p fennec-server -p fennec-cli -p fennec-shard
    PRE="$WORK/pre-target/debug"
fi

# The address a server said it listens on, waited for.
address() {
    local log=$1
    for _ in $(seq 1 200); do
        if grep -q 'listening on: http://' "$log" 2>/dev/null; then
            grep -o 'listening on: http://[^ ]*' "$log" | head -1 | sed 's/listening on: //'
            return
        fi
        sleep 0.05
    done
    fail "no listening line in $log"
}

# Waits up to 30 s for `pattern` in `log`.
said() {
    local log=$1 pattern=$2
    for _ in $(seq 1 600); do
        grep -q "$pattern" "$log" 2>/dev/null && return 0
        sleep 0.05
    done
    return 1
}

query() {
    curl -sS -H 'Authorization: Bearer t' -H 'Content-Type: application/json' \
        -d "{\"query\":\"$2\"}" "$1/query"
}

"$PRE/fennec-server" --file "$WORK/primary.fennec" --http 127.0.0.1:0 \
    --http-token t --replication-token r 2>"$WORK/primary.log" &
PIDS+=($!)
PRIMARY=$(address "$WORK/primary.log")

"$OLD/fennec-server" --file "$WORK/replica.fennec" --http 127.0.0.1:0 \
    --http-token t --replication-token r --replica-of "$PRIMARY" 2>"$WORK/replica.log" &
PIDS+=($!)
REPLICA=$(address "$WORK/replica.log")

"$OLD/fennec" archive "$PRIMARY" "$WORK/archive" --token r --image-every 1s \
    2>"$WORK/archiver.log" &
PIDS+=($!)

query "$PRIMARY" "create collection notes (title text)" >/dev/null
query "$PRIMARY" "put notes {title: 'before'}" >/dev/null
for _ in $(seq 1 600); do
    query "$REPLICA" "get notes count" | grep -q '"count":1' && break
    sleep 0.05
done
query "$REPLICA" "get notes count" | grep -q '"count":1' ||
    fail "the old replica never followed the primary"
said "$WORK/archiver.log" "image at change" || fail "the old archiver took no image"

# The first definition: the record an older reader has no kind for.
query "$PRIMARY" "create statement titles() as get notes select title" | grep -q '"error"' &&
    fail "the primary refused the definition"
query "$PRIMARY" "put notes {title: 'after'}" >/dev/null

said "$WORK/replica.log" "kind 13" || fail "the old replica said nothing of record kind 13"
query "$REPLICA" "get notes count" | grep -q '"count":1' ||
    fail "the old replica took a write past the record it refused"
said "$WORK/archiver.log" "kind 13" || fail "the old archiver said nothing of record kind 13"

echo "old replica:  $(grep -m1 'kind 13' "$WORK/replica.log")"
echo "old archiver: $(grep -m1 'kind 13' "$WORK/archiver.log")"

# What the old archiver kept, read by each `fennec verify`: the old one
# refuses the kind, this tree's reads it.
kill "${PIDS[2]}" 2>/dev/null || true
wait "${PIDS[2]}" 2>/dev/null || true
if TMPDIR="$WORK" "$OLD/fennec" verify "$WORK/archive" >"$WORK/verify-old.log" 2>&1; then
    fail "the old verify read an archive holding record kind 13"
fi
grep -q 'kind 13' "$WORK/verify-old.log" || fail "the old verify said nothing of record kind 13"
TMPDIR="$WORK" "$PRE/fennec" verify "$WORK/archive" >"$WORK/verify-new.log" 2>&1 ||
    fail "this tree's verify refused the archive the old archiver kept"
echo "old verify:   $(grep -m1 'kind 13' "$WORK/verify-old.log")"
echo "new verify:   $(head -1 "$WORK/verify-new.log")"

# A restore: the old `fennec restore` refuses the archive, this tree's
# writes the file, and the old server refuses to open what it wrote -- a
# node a tenant is restored to that still runs the binary from before.
if TMPDIR="$WORK" "$OLD/fennec" restore "$WORK/archive" "$WORK/restored-old.fennec" \
    >"$WORK/restore-old.log" 2>&1; then
    fail "the old restore read an archive holding record kind 13"
fi
grep -q 'kind 13' "$WORK/restore-old.log" || fail "the old restore said nothing of record kind 13"
TMPDIR="$WORK" "$PRE/fennec" restore "$WORK/archive" "$WORK/restored.fennec" \
    >"$WORK/restore-new.log" 2>&1 || fail "this tree's restore refused the archive"
"$OLD/fennec-server" --file "$WORK/restored.fennec" --http 127.0.0.1:0 \
    >"$WORK/restored-open.log" 2>&1 &
OPEN=$!
PIDS+=($OPEN)
for _ in $(seq 1 600); do
    kill -0 "$OPEN" 2>/dev/null || break
    sleep 0.05
done
kill -0 "$OPEN" 2>/dev/null && fail "the old server opened a file holding record kind 13"
grep -q 'kind 13' "$WORK/restored-open.log" || fail "the old server said nothing of record kind 13"
echo "old restore:  $(grep -m1 'kind 13' "$WORK/restore-old.log")"
echo "old open:     $(grep -m1 'kind 13' "$WORK/restored-open.log")"

# An admin request: its status printed, its body left in admin.out.
admin() {
    curl -sS -o "$WORK/admin.out" -w '%{http_code}' -X "$2" -H 'Authorization: Bearer root' \
        ${4:+-d "$4"} "$1$3"
}
SCHEMA='create collection notes (title text); create statement titles() as get notes select title'

# A router from before reads a tenant's schema before it places anything:
# one declaring a statement does not read there (400), and no tenant is
# left behind.
"$OLD/fennec-shard" --listen 127.0.0.1:0 --directory "$WORK/old-router.fennec" \
    --token root >"$WORK/old-router.log" 2>&1 &
PIDS+=($!)
OLD_ROUTER=$(address "$WORK/old-router.log")
code=$(admin "$OLD_ROUTER" PUT /_shard/tenants/acme "{\"schema\":\"$SCHEMA\"}")
[ "$code" = 400 ] ||
    fail "the old router answered $code for a schema declaring a statement: $(cat "$WORK/admin.out")"
echo "old router:   $code $(cat "$WORK/admin.out")"
code=$(admin "$OLD_ROUTER" GET /_shard/tenants/acme)
[ "$code" = 404 ] || fail "the old router placed the tenant it refused ($code)"

# A move onto a node from before: this tree's router places the tenant,
# its statement and all, on this tree's node; the move to the old node
# fails at its install, and the tenant stays on its source, answering what
# it held.
"$PRE/fennec-server" --dir "$WORK/n1" --http 127.0.0.1:0 --admin-token a1 \
    >"$WORK/n1.log" 2>&1 &
PIDS+=($!)
N1=$(address "$WORK/n1.log")
"$OLD/fennec-server" --dir "$WORK/n2" --http 127.0.0.1:0 --admin-token a2 \
    >"$WORK/n2.log" 2>&1 &
PIDS+=($!)
N2=$(address "$WORK/n2.log")
"$PRE/fennec-shard" --listen 127.0.0.1:0 --directory "$WORK/router.fennec" \
    --token root >"$WORK/router.log" 2>&1 &
ROUTER_PID=$!
PIDS+=($ROUTER_PID)
ROUTER=$(address "$WORK/router.log")
for n in "n1 ${N1#http://} a1" "n2 ${N2#http://} a2"; do
    set -- $n
    code=$(admin "$ROUTER" PUT "/_shard/nodes/$1" "{\"addr\":\"$2\",\"token\":\"$3\"}")
    case "$code" in
    2??) ;;
    *) fail "the router did not take $1 ($code): $(cat "$WORK/admin.out")" ;;
    esac
done
code=$(admin "$ROUTER" PUT /_shard/tenants/acme "{\"node\":\"n1\",\"schema\":\"$SCHEMA\"}")
case "$code" in
2??) ;;
*) fail "this tree's router did not place the tenant ($code): $(cat "$WORK/admin.out")" ;;
esac
query "$ROUTER/t/acme" "put notes {title: 'kept'}" | grep -q '"error"' &&
    fail "the tenant took no write"
code=$(admin "$ROUTER" POST /_shard/tenants/acme/move '{"to":"n2"}')
case "$code" in
2??) fail "the move onto the old node answered $code: $(cat "$WORK/admin.out")" ;;
esac
grep -q 'kind 13' "$WORK/admin.out" "$WORK/n2.log" ||
    fail "the failed move said nothing of record kind 13: $(cat "$WORK/admin.out")"
echo "old node:     $code $(cat "$WORK/admin.out")"
admin "$ROUTER" GET /_shard/tenants/acme >/dev/null
grep -q '"node":"n1"' "$WORK/admin.out" ||
    fail "the tenant left its source: $(cat "$WORK/admin.out")"
query "$ROUTER/t/acme" "get notes count" | grep -q '"count":1' ||
    fail "the tenant on its source lost what it held"

# A file left on a node from before identities is never taken back (roadmap
# row 47): such a node names no identity, so this tree's router notes none
# for its files -- the row here is put in its directory by hand, as one
# noted while the node ran a later binary -- and it answers a reclaim 404,
# which keeps the file: no DELETE in its place, the file reported.
code=$(curl -sS -o "$WORK/admin.out" -w '%{http_code}' -X PUT \
    -H 'Authorization: Bearer a2' "$N2/_admin/tenants/zzz")
[ "$code" = 201 ] || fail "the old node made no tenant ($code): $(cat "$WORK/admin.out")"
grep -q '"instance"' "$WORK/admin.out" &&
    fail "the old node named an identity: $(cat "$WORK/admin.out")"
query "$N2/t/zzz" "create collection notes (title text)" >/dev/null
query "$N2/t/zzz" "put notes {title: 'kept'}" >/dev/null
kill "$ROUTER_PID" 2>/dev/null || true
wait "$ROUTER_PID" 2>/dev/null || true
"$PRE/fennec" "$WORK/router.fennec" -c "put leftovers {key: 'zzz/n2', tenant: 'zzz', \
node: 'n2', why: 'a create taken back', noted: 1, instances: '00000000000000ab'}" \
    >"$WORK/seed.log" 2>&1 || fail "the directory took no row: $(cat "$WORK/seed.log")"
"$PRE/fennec-shard" --listen 127.0.0.1:0 --directory "$WORK/router.fennec" \
    --token root >"$WORK/router2.log" 2>&1 &
PIDS+=($!)
ROUTER=$(address "$WORK/router2.log")
admin "$ROUTER" POST /_shard/replicas >/dev/null
grep -q '"tenant":"zzz","node":"n2"' "$WORK/admin.out" ||
    fail "the file left on the old node went unreported: $(cat "$WORK/admin.out")"
grep -q 'answered 404' "$WORK/admin.out" ||
    fail "the old node was not asked, or answered otherwise: $(cat "$WORK/admin.out")"
query "$N2/t/zzz" "get notes count" | grep -q '"count":1' ||
    fail "the old node's file was taken back"
echo "old node:     a reclaim answered 404, the file kept and reported"

# A router from before identities over this tree's nodes (roadmap row 47):
# it proposes no identity and reads none, and every call it makes is
# answered as before -- a create and its replica's follow, each file given
# an identity the node minted in its role (`"instance"`, an answer's field
# the old router passes over), a move's freeze and install, and a delete
# taking each file and its `.instance` away.
for n in n3 n4 n5; do
    "$PRE/fennec-server" --dir "$WORK/$n" --http 127.0.0.1:0 --admin-token "a$n" \
        --replication-token r >"$WORK/$n.log" 2>&1 &
    PIDS+=($!)
done
"$OLD/fennec-shard" --listen 127.0.0.1:0 --directory "$WORK/old-router2.fennec" \
    --token root --replicas >"$WORK/old-router2.log" 2>&1 &
PIDS+=($!)
OLD_ROUTER=$(address "$WORK/old-router2.log")
for n in n3 n4 n5; do
    addr=$(address "$WORK/$n.log")
    code=$(admin "$OLD_ROUTER" PUT "/_shard/nodes/$n" "{\"addr\":\"${addr#http://}\",\"token\":\"a$n\"}")
    case "$code" in
    2??) ;;
    *) fail "the old router did not take $n ($code): $(cat "$WORK/admin.out")" ;;
    esac
done
code=$(admin "$OLD_ROUTER" PUT /_shard/tenants/beta '{"node":"n3"}')
case "$code" in
2??) ;;
*) fail "the old router placed no tenant on this tree's node ($code): $(cat "$WORK/admin.out")" ;;
esac
query "$OLD_ROUTER/t/beta" "create collection notes (title text)" >/dev/null
query "$OLD_ROUTER/t/beta" "put notes {title: 'held'}" | grep -q '"error"' &&
    fail "the tenant on this tree's node took no write"
admin "$OLD_ROUTER" GET /_shard/tenants/beta >/dev/null
REP=$(grep -o '"replica":"n[45]"' "$WORK/admin.out" | cut -d'"' -f4)
[ -n "$REP" ] || fail "the old router gave the tenant no replica: $(cat "$WORK/admin.out")"
grep -q ' n$' "$WORK/n3/beta.instance" ||
    fail "the create gave its file no identity: $(cat "$WORK/n3/beta.instance" 2>&1)"
grep -q ' f$' "$WORK/$REP/beta.instance" ||
    fail "the follow gave its copy no identity: $(cat "$WORK/$REP/beta.instance" 2>&1)"
TO=n4
[ "$REP" = n4 ] && TO=n5
code=$(admin "$OLD_ROUTER" POST /_shard/tenants/beta/move "{\"to\":\"$TO\"}")
case "$code" in
2??) ;;
*) fail "the old router's move between this tree's nodes answered $code: $(cat "$WORK/admin.out")" ;;
esac
query "$OLD_ROUTER/t/beta" "get notes count" | grep -q '"count":1' ||
    fail "the tenant moved by the old router lost what it held"
[ -e "$WORK/$TO/beta.instance" ] || fail "the install gave its file no identity"
[ -e "$WORK/n3/beta.fennec" ] || [ -e "$WORK/n3/beta.instance" ] &&
    fail "the move's source, or its identity, stayed on n3"
code=$(admin "$OLD_ROUTER" DELETE /_shard/tenants/beta)
[ "$code" = 204 ] || fail "the old router's delete answered $code: $(cat "$WORK/admin.out")"
for n in n3 n4 n5; do
    for f in beta.fennec beta.instance; do
        [ -e "$WORK/$n/$f" ] && fail "the delete left $n's $f"
    done
done
echo "old router:   a create, its replica, a move and a delete on this tree's nodes"

# A node serving HTTPS, recorded at its https:// address (TLS slice 3,
# docs/design/http.md#dialing-https). This tree's router dials it over TLS,
# the test CA its --tls-ca; a router from before cannot resolve such an
# address and sends the node nothing -- never its admin token in the
# clear, which a field beside the address it passed over would have sent;
# and a node from before refuses to follow an https:// primary. So every
# router and node is upgraded before a node is recorded at https://.
CERTS="$ROOT/crates/fennec-http/tests/certs"
"$PRE/fennec-server" --dir "$WORK/tls-node" --http 127.0.0.1:0 --admin-token atls \
    --tls-cert "$CERTS/server.pem" --tls-key "$CERTS/server.key" >"$WORK/tls-node.log" 2>&1 &
PIDS+=($!)
for _ in $(seq 1 200); do
    grep -q 'listening on: https://' "$WORK/tls-node.log" && break
    sleep 0.05
done
TLS_PORT=$(grep -o 'listening on: https://[^ ]*' "$WORK/tls-node.log" | head -1 | sed 's/.*://')
[ -n "$TLS_PORT" ] || fail "the node serving HTTPS said no address"
TLS_NODE="{\"addr\":\"https://localhost:$TLS_PORT\",\"token\":\"atls\"}"
plain_refused() {
    curl -sS --cacert "$CERTS/ca.pem" -H 'Authorization: Bearer atls' \
        "https://localhost:$TLS_PORT/_metrics" |
        grep 'fennec_tls_handshakes_failed_total{reason="plain_http"}' | awk '{print $2}'
}
"$PRE/fennec-shard" --listen 127.0.0.1:0 --directory "$WORK/tls-router.fennec" \
    --token root --tls-ca "$CERTS/ca.pem" >"$WORK/tls-router.log" 2>&1 &
PIDS+=($!)
NEW_ROUTER=$(address "$WORK/tls-router.log")
code=$(admin "$NEW_ROUTER" PUT /_shard/nodes/t1 "$TLS_NODE")
case "$code" in
2??) ;;
*) fail "this tree's router did not take the https node ($code): $(cat "$WORK/admin.out")" ;;
esac
code=$(admin "$NEW_ROUTER" PUT /_shard/tenants/secure '{"node":"t1"}')
[ "$code" = 201 ] || fail "no tenant on the https node ($code): $(cat "$WORK/admin.out")"
query "$NEW_ROUTER/t/secure" "create collection notes (title text)" | grep -q '"error"' &&
    fail "the router's relay to the https node failed"
before=$(plain_refused)
code=$(admin "$OLD_ROUTER" PUT /_shard/nodes/t1 "$TLS_NODE")
case "$code" in
2??) fail "the old router took an https:// address it cannot dial ($code)" ;;
esac
[ "$(plain_refused)" = "$before" ] ||
    fail "the old router sent the https node plain HTTP: $before -> $(plain_refused)"
echo "old router:   an https:// node refused ($code), nothing sent to it"
if "$OLD/fennec-server" --file "$WORK/tls-replica.fennec" --http 127.0.0.1:0 \
    --replication-token r --replica-of "https://localhost:$TLS_PORT" \
    >"$WORK/tls-replica.log" 2>&1; then
    fail "the old server followed an https:// primary"
fi
echo "old replica:  $(grep -m1 -i 'tls\|https' "$WORK/tls-replica.log")"
echo "ok: the binaries from before stop in words at the first definition"

# Op 3 (docs/design/named-statements.md, F2): a statement bound to a
# collection's writes is kind 13's op 3, whose collection and events stand
# outside its text. A binary from before it -- OP3_BASE, the last main
# before the op -- reads kind 13 and refuses an op it does not read, never
# taking the binding for a callable statement whose writes would land
# without its rows. This tree's primary defines one by `/query`, as a
# client does since the engine runs them (F2's slice 3):
#
#   - an old replica following it refuses the record in words, takes no
#     write past it, and connects again;
#   - an old archiver keeps the stream's bytes but takes no image of its
#     own; the old `fennec restore` refuses what it kept, this tree's
#     restores it, and the old server refuses to open the file it wrote;
#   - a move onto an old node fails at its install, the tenant left on its
#     source, answering, its binding running;
#   - the old server, an old replica of a server serving it and the old
#     `fennec types` refuse fennec-core's fixture, written by its tests with
#     a binding after F1's records (`tests/statements.fennec`).
OP3_BASE=${OP3_BASE:-b73ce4ca}
if [ -n "${OLD3_BIN_DIR:-}" ]; then
    OLD3=$OLD3_BIN_DIR
else
    echo "building fennec-server and fennec at $OP3_BASE ..."
    git -C "$ROOT" worktree add --detach --quiet "$WORK/old3" "$OP3_BASE"
    CARGO_TARGET_DIR="$WORK/old3-target" "${CARGO:-cargo}" build --quiet \
        --manifest-path "$WORK/old3/Cargo.toml" -p fennec-server -p fennec-cli
    OLD3="$WORK/old3-target/debug"
fi
UNREAD='a statement record this version does not read'
BINDING="create statement paid after set orders where new.status = 'paid' as insert ledger {o: new.id}"

"$PRE/fennec-server" --file "$WORK/p3.fennec" --http 127.0.0.1:0 \
    --http-token t --replication-token r 2>"$WORK/p3.log" &
PIDS+=($!)
P3=$(address "$WORK/p3.log")
"$OLD3/fennec-server" --file "$WORK/r3.fennec" --http 127.0.0.1:0 \
    --http-token t --replication-token r --replica-of "$P3" 2>"$WORK/r3.log" &
PIDS+=($!)
R3=$(address "$WORK/r3.log")
"$OLD3/fennec" archive "$P3" "$WORK/archive3" --token r --image-every 1s \
    2>"$WORK/archiver3.log" &
ARCHIVER3=$!
PIDS+=($ARCHIVER3)

query "$P3" "create collection orders (status text)" >/dev/null
query "$P3" "create collection ledger (o int)" >/dev/null
query "$P3" "put orders {status: 'open'}" >/dev/null
for _ in $(seq 1 600); do
    query "$R3" "get orders count" | grep -q '"count":1' && break
    sleep 0.05
done
query "$R3" "get orders count" | grep -q '"count":1' ||
    fail "the old replica never followed this tree's primary"
said "$WORK/archiver3.log" "image at change" || fail "the old archiver took no image"

# The first binding, defined as a client defines one, and a write it wakes.
query "$P3" "$BINDING" | grep -q '"error"' && fail "the primary refused the binding"
query "$P3" "set orders {status: 'paid'} where id = 1" | grep -q '"error"' &&
    fail "the primary refused the write its binding binds"
query "$P3" "get ledger count" | grep -q '"count":1' ||
    fail "the primary's write landed without its binding's row"

said "$WORK/r3.log" "$UNREAD" || fail "the old replica said nothing of op 3"
query "$R3" "get orders select status" | grep -q 'paid' &&
    fail "the old replica took a write past the binding it refused"
said "$WORK/archiver3.log" "$UNREAD" || fail "the old archiver said nothing of op 3"
echo "old replica:  $(grep -m1 "$UNREAD" "$WORK/r3.log")"
echo "old archiver: $(grep -m1 "$UNREAD" "$WORK/archiver3.log")"

kill "$ARCHIVER3" 2>/dev/null || true
wait "$ARCHIVER3" 2>/dev/null || true
if TMPDIR="$WORK" "$OLD3/fennec" restore "$WORK/archive3" "$WORK/restored3-old.fennec" \
    >"$WORK/restore3-old.log" 2>&1; then
    fail "the old restore read an archive holding op 3"
fi
grep -q "$UNREAD" "$WORK/restore3-old.log" || fail "the old restore said nothing of op 3"
TMPDIR="$WORK" "$PRE/fennec" restore "$WORK/archive3" "$WORK/restored3.fennec" \
    >"$WORK/restore3-new.log" 2>&1 || fail "this tree's restore refused the archive"
"$OLD3/fennec-server" --file "$WORK/restored3.fennec" --http 127.0.0.1:0 \
    >"$WORK/restored3-open.log" 2>&1 &
OPEN=$!
PIDS+=($OPEN)
for _ in $(seq 1 600); do
    kill -0 "$OPEN" 2>/dev/null || break
    sleep 0.05
done
kill -0 "$OPEN" 2>/dev/null && fail "the old server opened a restore holding op 3"
grep -q "$UNREAD" "$WORK/restored3-open.log" || fail "the old server said nothing of op 3"
echo "old restore:  $(grep -m1 "$UNREAD" "$WORK/restore3-old.log")"
echo "old open:     $(grep -m1 "$UNREAD" "$WORK/restored3-open.log")"

# A move onto an old node: this tree's router places the tenant, its
# binding and all, on this tree's node; the move to the old one fails at
# its install, and the tenant stays on its source, its binding running.
"$PRE/fennec-server" --dir "$WORK/n6" --http 127.0.0.1:0 --admin-token a6 \
    >"$WORK/n6.log" 2>&1 &
PIDS+=($!)
N6=$(address "$WORK/n6.log")
"$OLD3/fennec-server" --dir "$WORK/n7" --http 127.0.0.1:0 --admin-token a7 \
    >"$WORK/n7.log" 2>&1 &
PIDS+=($!)
N7=$(address "$WORK/n7.log")
"$PRE/fennec-shard" --listen 127.0.0.1:0 --directory "$WORK/router3.fennec" \
    --token root >"$WORK/router3.log" 2>&1 &
PIDS+=($!)
ROUTER3=$(address "$WORK/router3.log")
for n in "n6 ${N6#http://} a6" "n7 ${N7#http://} a7"; do
    set -- $n
    code=$(admin "$ROUTER3" PUT "/_shard/nodes/$1" "{\"addr\":\"$2\",\"token\":\"$3\"}")
    case "$code" in
    2??) ;;
    *) fail "the router did not take $1 ($code): $(cat "$WORK/admin.out")" ;;
    esac
done
code=$(admin "$ROUTER3" PUT /_shard/tenants/shop '{"node":"n6"}')
case "$code" in
2??) ;;
*) fail "this tree's router did not place the tenant ($code): $(cat "$WORK/admin.out")" ;;
esac
query "$ROUTER3/t/shop" "create collection orders (status text)" >/dev/null
query "$ROUTER3/t/shop" "create collection ledger (o int)" >/dev/null
query "$ROUTER3/t/shop" "$BINDING" | grep -q '"error"' && fail "the tenant refused the binding"
query "$ROUTER3/t/shop" "put orders {status: 'open'}" >/dev/null
code=$(admin "$ROUTER3" POST /_shard/tenants/shop/move '{"to":"n7"}')
case "$code" in
2??) fail "the move onto the old node answered $code: $(cat "$WORK/admin.out")" ;;
esac
grep -q "$UNREAD" "$WORK/admin.out" "$WORK/n7.log" ||
    fail "the failed move said nothing of op 3: $(cat "$WORK/admin.out")"
echo "old node:     $code $(cat "$WORK/admin.out")"
admin "$ROUTER3" GET /_shard/tenants/shop >/dev/null
grep -q '"node":"n6"' "$WORK/admin.out" ||
    fail "the tenant left its source: $(cat "$WORK/admin.out")"
query "$ROUTER3/t/shop" "set orders {status: 'paid'} where id = 1" | grep -q '"error"' &&
    fail "the tenant on its source took no write"
query "$ROUTER3/t/shop" "get ledger count" | grep -q '"count":1' ||
    fail "the tenant on its source wrote without its binding's row"

# The fixture: a file holding a binding after F1's records.
cp "$ROOT/crates/fennec-core/tests/statements.fennec" "$WORK/bound.fennec"
cp "$WORK/bound.fennec" "$WORK/bound-old.fennec"

"$OLD3/fennec-server" --file "$WORK/bound-old.fennec" --http 127.0.0.1:0 \
    >"$WORK/bound-open.log" 2>&1 &
OPEN=$!
PIDS+=($OPEN)
for _ in $(seq 1 600); do
    kill -0 "$OPEN" 2>/dev/null || break
    sleep 0.05
done
kill -0 "$OPEN" 2>/dev/null && fail "the old server opened a file holding op 3"
grep -q "$UNREAD" "$WORK/bound-open.log" || fail "the old server said nothing of op 3"
echo "old open:     $(grep -m1 "$UNREAD" "$WORK/bound-open.log")"

"$PRE/fennec-server" --file "$WORK/bound.fennec" --http 127.0.0.1:0 \
    --http-token t --replication-token r 2>"$WORK/bound-primary.log" &
PIDS+=($!)
BOUND=$(address "$WORK/bound-primary.log")
query "$BOUND" "get accounts count" | grep -q '"error"' &&
    fail "this tree's server answered no read of the file holding op 3"
query "$BOUND" "insert entries {src: 1, dst: 1, amount: 5}" | grep -q '"error"' &&
    fail "this tree's server refused a write its binding runs for"

"$OLD3/fennec-server" --file "$WORK/bound-replica.fennec" --http 127.0.0.1:0 \
    --http-token t --replication-token r --replica-of "$BOUND" 2>"$WORK/bound-replica.log" &
PIDS+=($!)
BOUND_REPLICA=$(address "$WORK/bound-replica.log")
said "$WORK/bound-replica.log" "$UNREAD" || fail "the old replica said nothing of op 3"
query "$BOUND_REPLICA" "get accounts count" | grep -q '"count"' &&
    fail "the old replica took the image holding op 3"
echo "old replica:  $(grep -m1 "$UNREAD" "$WORK/bound-replica.log")"

if "$OLD3/fennec" types "$WORK/bound-old.fennec" >"$WORK/bound-types.log" 2>&1; then
    fail "the old \`fennec types\` read a file holding op 3"
fi
grep -q "$UNREAD" "$WORK/bound-types.log" || fail "the old \`fennec types\` said nothing of op 3"
echo "old types:    $(grep -m1 "$UNREAD" "$WORK/bound-types.log")"
echo "ok: the binaries from before stop in words at a binding (op 3)"

# Ops 4 and 5 (docs/design/named-statements.md, F3): a statement run by the
# clock is kind 13's op 4, its schedule and `since` outside its text, and
# the mark of a slot it ran for op 5. A binary from before them --
# OP4_BASE, the last main before the ops -- refuses an op it does not read,
# never taking a schedule for a callable statement nobody runs.
# fennec-core's fixture holds one and a mark, written by its tests
# (`tests/statements.fennec`), and
#
#   - the old server refuses to open it, in words;
#   - this tree's server opens it, answers its reads, takes its writes and
#     runs the schedule by the server's token's call;
#   - an old replica of that server refuses the image holding op 4, and an
#     old archiver takes no image of its own;
#   - the old `fennec types` refuses it.
#
# And this tree's primary defines one by `/query`, as a client does since a
# file's server runs them (F3's slice 3), and runs its slots:
#
#   - an old replica following it refuses the record in words, takes no
#     write past it, and connects again;
#   - an old archiver keeps the stream's bytes but takes no image of its
#     own; the old `fennec restore` refuses what it kept, this tree's
#     restores it, and the old server refuses to open the file it wrote.
#
# A tenant node takes no schedule until F3's slice 4, which drives a move.
OP4_BASE=${OP4_BASE:-cd907d6c}
if [ -n "${OLD4_BIN_DIR:-}" ]; then
    OLD4=$OLD4_BIN_DIR
else
    echo "building fennec-server and fennec at $OP4_BASE ..."
    git -C "$ROOT" worktree add --detach --quiet "$WORK/old4" "$OP4_BASE"
    CARGO_TARGET_DIR="$WORK/old4-target" "${CARGO:-cargo}" build --quiet \
        --manifest-path "$WORK/old4/Cargo.toml" -p fennec-server -p fennec-cli
    OLD4="$WORK/old4-target/debug"
fi
cp "$ROOT/crates/fennec-core/tests/statements.fennec" "$WORK/scheduled.fennec"
cp "$WORK/scheduled.fennec" "$WORK/scheduled-old.fennec"

"$OLD4/fennec-server" --file "$WORK/scheduled-old.fennec" --http 127.0.0.1:0 \
    >"$WORK/scheduled-open.log" 2>&1 &
OPEN=$!
PIDS+=($OPEN)
for _ in $(seq 1 600); do
    kill -0 "$OPEN" 2>/dev/null || break
    sleep 0.05
done
kill -0 "$OPEN" 2>/dev/null && fail "the old server opened a file holding op 4"
grep -q "$UNREAD" "$WORK/scheduled-open.log" || fail "the old server said nothing of op 4"
echo "old open:     $(grep -m1 "$UNREAD" "$WORK/scheduled-open.log")"

"$PRE/fennec-server" --file "$WORK/scheduled.fennec" --http 127.0.0.1:0 \
    --http-token t --replication-token r 2>"$WORK/scheduled-primary.log" &
PIDS+=($!)
SCHEDULED=$(address "$WORK/scheduled-primary.log")
query "$SCHEDULED" "get holds select state" | grep -q 'lapsed' ||
    fail "this tree's server answered no read of the file holding op 4"
query "$SCHEDULED" "call lapse" | grep -q '"error"' &&
    fail "this tree's server refused its token's call of a schedule"

# The old archiver keeps the primary's image as it came; the image of its
# own it takes once a write follows reads op 4.
"$OLD4/fennec" archive "$SCHEDULED" "$WORK/archive4" --token r --image-every 1s \
    2>"$WORK/archiver4.log" &
ARCHIVER4=$!
PIDS+=($ARCHIVER4)
said "$WORK/archiver4.log" "image at change" || fail "the old archiver took no image"
query "$SCHEDULED" "put holds {state: 'held'}" | grep -q '"error"' &&
    fail "this tree's server refused a write beside a schedule"
said "$WORK/archiver4.log" "$UNREAD" || fail "the old archiver said nothing of op 4"
echo "old archiver: $(grep -m1 "$UNREAD" "$WORK/archiver4.log")"
kill "$ARCHIVER4" 2>/dev/null || true
wait "$ARCHIVER4" 2>/dev/null || true

"$OLD4/fennec-server" --file "$WORK/scheduled-replica.fennec" --http 127.0.0.1:0 \
    --http-token t --replication-token r --replica-of "$SCHEDULED" \
    2>"$WORK/scheduled-replica.log" &
PIDS+=($!)
SCHEDULED_REPLICA=$(address "$WORK/scheduled-replica.log")
said "$WORK/scheduled-replica.log" "$UNREAD" || fail "the old replica said nothing of op 4"
query "$SCHEDULED_REPLICA" "get holds count" | grep -q '"count"' &&
    fail "the old replica took the image holding op 4"
echo "old replica:  $(grep -m1 "$UNREAD" "$WORK/scheduled-replica.log")"

if "$OLD4/fennec" types "$WORK/scheduled-old.fennec" >"$WORK/scheduled-types.log" 2>&1; then
    fail "the old \`fennec types\` read a file holding op 4"
fi
grep -q "$UNREAD" "$WORK/scheduled-types.log" || fail "the old \`fennec types\` said nothing of op 4"
echo "old types:    $(grep -m1 "$UNREAD" "$WORK/scheduled-types.log")"

# The first schedule, defined as a client defines one, beside an old
# replica and an old archiver following the primary that defines it.
SCHEDULE="create statement tidy every 1d as del orders where status = 'void'"
"$PRE/fennec-server" --file "$WORK/p4.fennec" --http 127.0.0.1:0 \
    --http-token t --replication-token r 2>"$WORK/p4.log" &
PIDS+=($!)
P4=$(address "$WORK/p4.log")
"$OLD4/fennec-server" --file "$WORK/r4.fennec" --http 127.0.0.1:0 \
    --http-token t --replication-token r --replica-of "$P4" 2>"$WORK/r4.log" &
PIDS+=($!)
R4=$(address "$WORK/r4.log")
"$OLD4/fennec" archive "$P4" "$WORK/archive4b" --token r --image-every 1s \
    2>"$WORK/archiver4b.log" &
ARCHIVER4B=$!
PIDS+=($ARCHIVER4B)
query "$P4" "create collection orders (status text @hash)" >/dev/null
query "$P4" "put orders {status: 'open'}" >/dev/null
for _ in $(seq 1 600); do
    query "$R4" "get orders count" | grep -q '"count":1' && break
    sleep 0.05
done
query "$R4" "get orders count" | grep -q '"count":1' ||
    fail "the old replica never followed this tree's primary"
said "$WORK/archiver4b.log" "image at change" || fail "the old archiver took no image"
query "$P4" "$SCHEDULE" | grep -q '"error"' && fail "the primary refused the schedule"
query "$P4" "put orders {status: 'paid'}" | grep -q '"error"' &&
    fail "the primary refused a write beside its schedule"
said "$WORK/r4.log" "$UNREAD" || fail "the old replica said nothing of op 4"
query "$R4" "get orders count" | grep -q '"count":2' &&
    fail "the old replica took a write past the schedule it refused"
said "$WORK/archiver4b.log" "$UNREAD" || fail "the old archiver said nothing of op 4"
echo "old replica:  $(grep -m1 "$UNREAD" "$WORK/r4.log")"
echo "old archiver: $(grep -m1 "$UNREAD" "$WORK/archiver4b.log")"
kill "$ARCHIVER4B" 2>/dev/null || true
wait "$ARCHIVER4B" 2>/dev/null || true
if TMPDIR="$WORK" "$OLD4/fennec" restore "$WORK/archive4b" "$WORK/restored4-old.fennec" \
    >"$WORK/restore4-old.log" 2>&1; then
    fail "the old restore read an archive holding op 4"
fi
grep -q "$UNREAD" "$WORK/restore4-old.log" || fail "the old restore said nothing of op 4"
TMPDIR="$WORK" "$PRE/fennec" restore "$WORK/archive4b" "$WORK/restored4.fennec" \
    >"$WORK/restore4-new.log" 2>&1 || fail "this tree's restore refused the archive"
"$OLD4/fennec-server" --file "$WORK/restored4.fennec" --http 127.0.0.1:0 \
    >"$WORK/restored4-open.log" 2>&1 &
OPEN=$!
PIDS+=($OPEN)
for _ in $(seq 1 600); do
    kill -0 "$OPEN" 2>/dev/null || break
    sleep 0.05
done
kill -0 "$OPEN" 2>/dev/null && fail "the old server opened a restore holding op 4"
grep -q "$UNREAD" "$WORK/restored4-open.log" || fail "the old server said nothing of op 4"
echo "old restore:  $(grep -m1 "$UNREAD" "$WORK/restore4-old.log")"
echo "old open:     $(grep -m1 "$UNREAD" "$WORK/restored4-open.log")"
echo "ok: the binaries from before stop in words at a schedule (op 4) and its mark (op 5)"

# Op 6 (docs/design/named-statements.md, F4): a webhook consumer is kind
# 13's op 6, its hook's name after its text. A binary from before it --
# OP6_BASE, the last main before the op -- refuses an op it does not read,
# never taking a hook's statement for a callable one whose text does not
# read, marked broken, its sender answered 404 for days. A file's server
# defines one from F4's slice 3, whose route takes its events: the upgrade
# order -- every reader before the first hook -- keeps a reader from
# meeting one it stops at. fennec-core's fixture holds one and an event it
# took, its write and its key one block, written by its tests
# (`tests/statements.fennec`), and
#
#   - the old server refuses to open it, in words;
#   - this tree's server opens it, answers its reads, takes its writes,
#     refuses a call of the hook's statement that carries no event and
#     defines another over `/query`, as a primary may from slice 3;
#   - an old replica of that server refuses the image holding op 6, and an
#     old archiver takes no image of its own;
#   - the old `fennec types` refuses it.
OP6_BASE=${OP6_BASE:-6fb28002}
if [ -n "${OLD6_BIN_DIR:-}" ]; then
    OLD6=$OLD6_BIN_DIR
else
    echo "building fennec-server and fennec at $OP6_BASE ..."
    git -C "$ROOT" worktree add --detach --quiet "$WORK/old6" "$OP6_BASE"
    CARGO_TARGET_DIR="$WORK/old6-target" "${CARGO:-cargo}" build --quiet \
        --manifest-path "$WORK/old6/Cargo.toml" -p fennec-server -p fennec-cli
    OLD6="$WORK/old6-target/debug"
fi
cp "$ROOT/crates/fennec-core/tests/statements.fennec" "$WORK/hooked.fennec"
cp "$WORK/hooked.fennec" "$WORK/hooked-old.fennec"

"$OLD6/fennec-server" --file "$WORK/hooked-old.fennec" --http 127.0.0.1:0 \
    >"$WORK/hooked-open.log" 2>&1 &
OPEN=$!
PIDS+=($OPEN)
for _ in $(seq 1 600); do
    kill -0 "$OPEN" 2>/dev/null || break
    sleep 0.05
done
kill -0 "$OPEN" 2>/dev/null && fail "the old server opened a file holding op 6"
grep -q "$UNREAD" "$WORK/hooked-open.log" || fail "the old server said nothing of op 6"
echo "old open:     $(grep -m1 "$UNREAD" "$WORK/hooked-open.log")"

"$PRE/fennec-server" --file "$WORK/hooked.fennec" --http 127.0.0.1:0 \
    --http-token t --replication-token r 2>"$WORK/hooked-primary.log" &
PIDS+=($!)
HOOKED=$(address "$WORK/hooked-primary.log")
query "$HOOKED" "get orders select status where ref = 'o-1'" | grep -q 'paid' ||
    fail "this tree's server answered no read of the file holding op 6"
query "$HOOKED" "call stripe_paid" | grep -q "takes a hook's events" ||
    fail "this tree's server ran a hook's statement by a call with no event"
query "$HOOKED" "create statement stripe_refunded(\$order text from event.data.object.metadata.order) on hook stripe as set orders {status: 'refunded'} where ref = \$order" |
    grep -q '"error"' && fail "this tree's server refused a hook's definition over /query"

# The old archiver keeps the primary's image as it came; the image of its
# own it takes once a write follows reads op 6.
"$OLD6/fennec" archive "$HOOKED" "$WORK/archive6" --token r --image-every 1s \
    2>"$WORK/archiver6.log" &
ARCHIVER6=$!
PIDS+=($ARCHIVER6)
said "$WORK/archiver6.log" "image at change" || fail "the old archiver took no image"
query "$HOOKED" "put orders {ref: 'o-2', status: 'open'}" | grep -q '"error"' &&
    fail "this tree's server refused a write beside a hook"
said "$WORK/archiver6.log" "$UNREAD" || fail "the old archiver said nothing of op 6"
echo "old archiver: $(grep -m1 "$UNREAD" "$WORK/archiver6.log")"
kill "$ARCHIVER6" 2>/dev/null || true
wait "$ARCHIVER6" 2>/dev/null || true

"$OLD6/fennec-server" --file "$WORK/hooked-replica.fennec" --http 127.0.0.1:0 \
    --http-token t --replication-token r --replica-of "$HOOKED" \
    2>"$WORK/hooked-replica.log" &
PIDS+=($!)
HOOKED_REPLICA=$(address "$WORK/hooked-replica.log")
said "$WORK/hooked-replica.log" "$UNREAD" || fail "the old replica said nothing of op 6"
query "$HOOKED_REPLICA" "get orders count" | grep -q '"count"' &&
    fail "the old replica took the image holding op 6"
echo "old replica:  $(grep -m1 "$UNREAD" "$WORK/hooked-replica.log")"

if "$OLD6/fennec" types "$WORK/hooked-old.fennec" >"$WORK/hooked-types.log" 2>&1; then
    fail "the old \`fennec types\` read a file holding op 6"
fi
grep -q "$UNREAD" "$WORK/hooked-types.log" || fail "the old \`fennec types\` said nothing of op 6"
echo "old types:    $(grep -m1 "$UNREAD" "$WORK/hooked-types.log")"
echo "ok: the binaries from before stop in words at a webhook consumer (op 6)"

# Record checksums (docs/design/storage.md#file-format): every record this
# tree writes ends in its CRC32C, its kind's high bit set and its length
# counting the sum. A binary from before them -- CK_BASE -- frames such a
# record by its length and refuses its kind in words, never reading the
# sum as a record; nothing has to be defined first, a primary's first
# write is one (RELEASING.md):
#
#   - an old replica of this tree's primary stops at what it is sent and
#     holds none of it;
#   - an old archiver keeps the stream and takes no image of its own, the
#     old `fennec verify` refusing what it kept;
#   - the old server and the old `fennec types` refuse the primary's file.
#
# And the order the upgrade takes, the other way: this tree's replica
# follows an old primary, holding its records as they came, and this
# tree's server opens the old primary's file and writes to it.
SUMMED='record kind 1[2-4][0-9]'
"$NEW/fennec-server" --file "$WORK/pc.fennec" --http 127.0.0.1:0 \
    --http-token t --replication-token r 2>"$WORK/pc.log" &
PC_PID=$!
PIDS+=($PC_PID)
PC=$(address "$WORK/pc.log")
"$PRE/fennec" archive "$PC" "$WORK/archivec" --token r --image-every 1s \
    2>"$WORK/archiverc.log" &
ARCHIVERC=$!
PIDS+=($ARCHIVERC)
query "$PC" "create collection notes (title text)" | grep -q '"error"' &&
    fail "this tree's primary refused a create"
query "$PC" "put notes {title: 'summed'}" | grep -q '"error"' &&
    fail "this tree's primary refused a write"
"$PRE/fennec-server" --file "$WORK/rc.fennec" --http 127.0.0.1:0 \
    --http-token t --replication-token r --replica-of "$PC" 2>"$WORK/rc.log" &
PIDS+=($!)
RC=$(address "$WORK/rc.log")
said "$WORK/rc.log" "$SUMMED" || fail "the old replica said nothing of a checked record"
query "$RC" "get notes count" | grep -q '"count":1' &&
    fail "the old replica took a checked record"
echo "old replica:  $(grep -m1 "$SUMMED" "$WORK/rc.log")"
query "$PC" "put notes {title: 'again'}" | grep -q '"error"' &&
    fail "this tree's primary refused a write"
said "$WORK/archiverc.log" "$SUMMED" || fail "the old archiver said nothing of a checked record"
echo "old archiver: $(grep -m1 "$SUMMED" "$WORK/archiverc.log")"
kill "$ARCHIVERC" 2>/dev/null || true
wait "$ARCHIVERC" 2>/dev/null || true
if TMPDIR="$WORK" "$PRE/fennec" verify "$WORK/archivec" >"$WORK/verifyc-old.log" 2>&1; then
    fail "the old verify read an archive of checked records"
fi
echo "old verify:   $(head -1 "$WORK/verifyc-old.log")"
kill "$PC_PID" 2>/dev/null || true
wait "$PC_PID" 2>/dev/null || true
"$PRE/fennec-server" --file "$WORK/pc.fennec" --http 127.0.0.1:0 \
    >"$WORK/pc-old-open.log" 2>&1 &
OPEN=$!
PIDS+=($OPEN)
for _ in $(seq 1 600); do
    kill -0 "$OPEN" 2>/dev/null || break
    sleep 0.05
done
kill -0 "$OPEN" 2>/dev/null && fail "the old server opened a file of checked records"
grep -q "$SUMMED" "$WORK/pc-old-open.log" || fail "the old server said nothing of a checked record"
echo "old open:     $(grep -m1 "$SUMMED" "$WORK/pc-old-open.log")"
if "$PRE/fennec" types "$WORK/pc.fennec" >"$WORK/pc-types.log" 2>&1; then
    fail "the old \`fennec types\` read a file of checked records"
fi
grep -q "$SUMMED" "$WORK/pc-types.log" || fail "the old \`fennec types\` said nothing of a checked record"
echo "old types:    $(grep -m1 "$SUMMED" "$WORK/pc-types.log")"

# The upgrade's own order: this tree's replica of an old primary, then this
# tree's server on the old primary's file.
"$PRE/fennec-server" --file "$WORK/po.fennec" --http 127.0.0.1:0 \
    --http-token t --replication-token r 2>"$WORK/po.log" &
PO_PID=$!
PIDS+=($PO_PID)
PO=$(address "$WORK/po.log")
query "$PO" "create collection notes (title text)" >/dev/null
query "$PO" "put notes {title: 'plain'}" >/dev/null
"$NEW/fennec-server" --file "$WORK/ro.fennec" --http 127.0.0.1:0 \
    --http-token t --replication-token r --replica-of "$PO" 2>"$WORK/ro.log" &
RO_PID=$!
PIDS+=($RO_PID)
RO=$(address "$WORK/ro.log")
query "$PO" "put notes {title: 'plain too'}" >/dev/null
for _ in $(seq 1 600); do
    query "$RO" "get notes count" | grep -q '"count":2' && break
    sleep 0.05
done
query "$RO" "get notes count" | grep -q '"count":2' ||
    fail "this tree's replica did not follow the old primary: $(tail -3 "$WORK/ro.log")"
kill "$RO_PID" "$PO_PID" 2>/dev/null || true
wait "$RO_PID" "$PO_PID" 2>/dev/null || true
"$NEW/fennec-server" --file "$WORK/po.fennec" --http 127.0.0.1:0 \
    --http-token t 2>"$WORK/po-new.log" &
PIDS+=($!)
PN=$(address "$WORK/po-new.log")
query "$PN" "put notes {title: 'summed'}" | grep -q '"error"' &&
    fail "this tree's server took no write on the old primary's file"
query "$PN" "get notes count" | grep -q '"count":3' ||
    fail "this tree's server lost a write of the old primary's file"
echo "new replica:  followed the old primary; this tree's server opened its file and wrote to it"
echo "ok: the binaries from before stop in words at a checked record"
