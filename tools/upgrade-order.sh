#!/usr/bin/env bash
# The upgrade's order for named statements (docs/design/named-statements.md,
# slice 4; RELEASING.md): readers first. A binary built before record kind 13
# -- a replica following a primary's stream, an archiver writing it -- meets
# the first `create statement` on a primary and refuses it in words, never
# misreading it: the replica refuses the write ("record kind 13 is not a
# write") and connects again, never taking one past it; the archiver keeps
# the stream's bytes but takes no image of its own ("unknown record kind
# 13"), and the old `fennec verify` refuses what it kept, which this tree's
# reads. This builds `fennec-server` and `fennec` at BASE, the last main
# before the kind, runs this tree's server as the primary and the old
# binaries beside it, defines a statement, and holds each to what it said:
#
#   tools/upgrade-order.sh NEW_BIN_DIR [BASE]
#
# NEW_BIN_DIR holds this tree's `fennec-server` and `fennec` (`make
# upgrade-check` builds them). OLD_BIN_DIR, where set, holds the old ones
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
    echo "building fennec-server and fennec at $BASE ..."
    git -C "$ROOT" worktree add --detach --quiet "$WORK/old" "$BASE"
    CARGO_TARGET_DIR="$WORK/old-target" "${CARGO:-cargo}" build --quiet \
        --manifest-path "$WORK/old/Cargo.toml" -p fennec-server -p fennec-cli
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
echo "ok: the readers from before stop in words at the first definition"
