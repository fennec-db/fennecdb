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
#     its source, answering, with what it held.
#
# This builds `fennec-server`, `fennec` and `fennec-shard` at BASE, the last
# main before the kind, runs this tree's binaries beside the old ones,
# defines a statement, and holds each old one to what it said:
#
#   tools/upgrade-order.sh NEW_BIN_DIR [BASE]
#
# NEW_BIN_DIR holds this tree's `fennec-server`, `fennec` and `fennec-shard`
# (`make upgrade-check` builds them). OLD_BIN_DIR, where set, holds the old ones
# already built and skips their build. Every server listens on port 0, found
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
    if [ -d "$WORK/old" ]; then
        git -C "$ROOT" worktree remove --force "$WORK/old" 2>/dev/null || true
    fi
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

"$NEW/fennec-server" --file "$WORK/primary.fennec" --http 127.0.0.1:0 \
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
TMPDIR="$WORK" "$NEW/fennec" verify "$WORK/archive" >"$WORK/verify-new.log" 2>&1 ||
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
TMPDIR="$WORK" "$NEW/fennec" restore "$WORK/archive" "$WORK/restored.fennec" \
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
"$NEW/fennec-server" --dir "$WORK/n1" --http 127.0.0.1:0 --admin-token a1 \
    >"$WORK/n1.log" 2>&1 &
PIDS+=($!)
N1=$(address "$WORK/n1.log")
"$OLD/fennec-server" --dir "$WORK/n2" --http 127.0.0.1:0 --admin-token a2 \
    >"$WORK/n2.log" 2>&1 &
PIDS+=($!)
N2=$(address "$WORK/n2.log")
"$NEW/fennec-shard" --listen 127.0.0.1:0 --directory "$WORK/router.fennec" \
    --token root >"$WORK/router.log" 2>&1 &
PIDS+=($!)
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
echo "ok: the binaries from before stop in words at the first definition"
