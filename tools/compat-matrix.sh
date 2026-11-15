#!/usr/bin/env bash
# Two releases' processes beside each other (docs/design/compatibility.md,
# "The compatibility matrix"): the last published release's binaries,
# OLD_BIN_DIR, as tools/release-bins.sh takes them from its assets, and this
# tree's, NEW_BIN_DIR, each case driven both ways where it has two:
#
#   tools/compat-matrix.sh OLD_BIN_DIR NEW_BIN_DIR
#
# Each directory holds `fennec-server`, `fennec` and `fennec-shard`
# (`make compat-check` builds this tree's and takes the release's). The
# policy it holds is RELEASING.md's (*Every reader upgrades before any
# primary*): every reader of a file or a stream -- replica, standby,
# archiver, router, node, the `fennec` CLI -- upgrades before any primary; a
# binary one release back beside a newer one works or stops in words naming
# what it does not know (a record kind, a tag, a version), and never reads
# wrong; a newer reader over an older writer always works. The cases:
#
#   a1  an old primary and a new replica: the replica follows, its image and
#       its records, its reads the primary's, its file opened again whole;
#   a2  a new primary and an old replica: it follows until a write it does
#       not know, where it stops in words, and its file, opened by the old
#       binary, holds exactly a prefix of the primary's history (every
#       snapshot taken, one after each write, is a candidate) -- each record
#       kind, tag or op added since the release is a write of HISTORY, named
#       in UNKNOWN_TO_OLD;
#   a3  an old primary's value nested past this tree's 64 levels, followed
#       by a new replica;
#   a4  what the old release writes and this tree no longer takes -- a
#       stored definition holding 1e400, a vector component of 1e39 --
#       followed by a new replica and read there as the old primary reads it;
#   b1-b4  archives: the old `fennec archive`'s, plain and sealed (version 1),
#       verified and restored by the new `fennec`, to the end and to a moment;
#       the new archiver's, plain, read by the old `fennec`, and sealed
#       (version 2), refused by it in RELEASING.md's words;
#   c1-c3  the old `fennec backup`, plain and sealed, opened by the new
#       server; a file the old server wrote, opened and written by the new,
#       then verified, typed and opened by the old; the new backup opened, or
#       refused in words, by the old;
#   d1-d2  an old router over new nodes and a new router over old nodes: a
#       tenant created, written, copied to its replica, moved and deleted;
#   e   a rolling upgrade under durable writes: a router and two `--dir`
#       nodes, one the other's standby, all of the release, upgraded in
#       RELEASING.md's and deploy/RUNBOOK.md's order -- the router in place,
#       the standby's node, then the primary's -- every acknowledged write
#       counted and read back at the end;
#   f1-f2  a `github` hook kept with `key delivery`, which RELEASING.md says
#       the release refuses: a single server's hooks file line, and a
#       tenant's set kept by an old node, the router and node upgraded;
#   g1-g2  a node serving HTTPS, recorded at https:// by a router of the
#       other release.
#
# COMPAT_CASES, where set, names the cases to run (`a2 e`): a case that
# failed under load is run again alone before it is believed.
#
# Each case prints one line, `ok` and what it held, or what differed; the
# script exits 1 where any differed. A difference is one of three: promised
# in RELEASING.md or compatibility.md, which the case asserts in the promised
# words; a break nobody wrote down; or a bug. The last two fail the case
# until someone decides: nothing here passes over one. Every server listens on
# port 0, found by its `listening on:` line (a node restarted in case e takes
# the port it was first given, as a machine keeps its address); everything
# started is killed and every path made removed on the way out, and a
# failure prints the logs' tails. COMPAT_LOGS, where set, is a directory the
# logs are copied into before they go. Needs curl, jq and openssl.

set -uo pipefail
# One collation for sort and comm, which reads its lines in sort's order.
export LC_ALL=C

usage='usage: tools/compat-matrix.sh OLD_BIN_DIR NEW_BIN_DIR'
ROOT=$(cd "$(dirname "$0")/.." && pwd)
OLD=$(cd "${1:?$usage}" && pwd)
NEW=$(cd "${2:?$usage}" && pwd)
for dir in "$OLD" "$NEW"; do
    for b in fennec-server fennec fennec-shard; do
        [ -x "$dir/$b" ] || { echo "compat-matrix: $dir holds no $b" >&2; exit 2; }
    done
done
for tool in curl jq openssl; do
    command -v "$tool" >/dev/null || { echo "compat-matrix: needs $tool" >&2; exit 2; }
done
OLD_SAYS=$("$OLD/fennec-server" --version 2>&1 | head -1)
NEW_SAYS=$("$NEW/fennec-server" --version 2>&1 | head -1)

WORK=$(mktemp -d "${TMPDIR:-/tmp}/fennec-compat.XXXXXX")
: >"$WORK/pids"

# Stops each process: TERM, a server's own shutdown, then KILL after 20 s.
stop() {
    local p
    for p in "$@"; do kill "$p" 2>/dev/null || true; done
    for p in "$@"; do
        for _ in $(seq 1 400); do
            kill -0 "$p" 2>/dev/null || break
            sleep 0.05
        done
        kill -9 "$p" 2>/dev/null || true
    done
}

cleanup() {
    # shellcheck disable=SC2046 # one pid a word
    stop $(cat "$WORK/pids" 2>/dev/null)
    if [ -n "${COMPAT_LOGS:-}" ]; then
        mkdir -p "$COMPAT_LOGS"
        (cd "$WORK" && find . \( -name '*.log' -o -name '*.out' -o -name lines \) -type f) |
            while read -r f; do
                mkdir -p "$COMPAT_LOGS/$(dirname "$f")"
                cp "$WORK/$f" "$COMPAT_LOGS/$f"
            done
    fi
    rm -rf "$WORK"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# --- What a case does with ---------------------------------------------------

# The line a case or a part of it ends on.
held() { echo "$1 ok: $2" >>"$C/lines"; }
differs() { echo "$1 FAILED: $2" >>"$C/lines"; }
# A difference that ends the case where it stands.
fail() {
    echo "$CASE FAILED: $*" >>"$C/lines"
    exit 1
}

# spawn LOG CMD...: started in the background, its pid kept for the way out.
spawn() {
    local log=$1
    shift
    "$@" >"$log" 2>&1 &
    SPAWNED=$!
    echo "$SPAWNED" >>"$WORK/pids"
    echo "$SPAWNED" >>"$C/pids"
}

# The address a server said it listens on, waited for.
addr() {
    local log=$1 pid=$2
    for _ in $(seq 1 600); do
        if grep -q 'listening on: http://' "$log" 2>/dev/null; then
            grep -o 'listening on: http://[^ ]*' "$log" | head -1 | sed 's/listening on: //'
            return 0
        fi
        kill -0 "$pid" 2>/dev/null ||
            fail "$(basename "$log" .log) exited before it listened: $(tail -3 "$log" | tr '\n' ' ')"
        sleep 0.05
    done
    fail "$(basename "$log" .log) said no address in 30 s"
}

# server NAME BIN_DIR ARGS...: a fennec-server on port 0; ADDR and PID set.
server() {
    local name=$1 bin=$2
    shift 2
    spawn "$C/$name.log" "$bin/fennec-server" --http 127.0.0.1:0 "$@"
    PID=$SPAWNED
    ADDR=$(addr "$C/$name.log" "$PID") || exit 1
}

# router NAME BIN_DIR ARGS...: a fennec-shard on port 0; ADDR and PID set.
router() {
    local name=$1 bin=$2
    shift 2
    spawn "$C/$name.log" "$bin/fennec-shard" --listen 127.0.0.1:0 "$@"
    PID=$SPAWNED
    ADDR=$(addr "$C/$name.log" "$PID") || exit 1
}

# Waits up to 30 s for `pattern` in `log`.
said() {
    for _ in $(seq 1 600); do
        grep -Eq "$2" "$1" 2>/dev/null && return 0
        sleep 0.05
    done
    return 1
}

# q URL QUERY [TOKEN]: the answer's body.
q() {
    curl -sS --max-time 60 -H "Authorization: Bearer ${3:-t}" -H 'Content-Type: application/json' \
        -d "$(jq -cn --arg q "$2" '{query: $q}')" "$1/query"
}

# w URL QUERY [TOKEN]: a write the case needs, its refusal the case's end.
w() {
    local out
    out=$(q "$@") || fail "\`$2\` had no answer from $1"
    case $out in
    *'"error"'*) fail "\`$2\` was refused by $1: $out" ;;
    esac
}

# admin URL METHOD PATH [BODY] [TOKEN]: the status; the body in $C/admin.out.
admin() {
    curl -sS --max-time 60 -o "$C/admin.out" -w '%{http_code}' -X "$2" \
        -H "Authorization: Bearer ${5:-root}" ${4:+-d "$4"} "$1$3"
}

# The history every case writes: a record of each kind a write makes, kind
# 13's ops 1, 3, 4 and 6 (a definition, a binding, a schedule whose slot
# deletes nothing, a webhook consumer), tag 17 (`nocase`) and tag 18
# (`@text` beside a `@hash`) among them. v0.2.0 reads every record kind, tag and op this tree writes: a
# release that adds one adds a write making it here, and its index to
# UNKNOWN_TO_OLD, where case a2 asks the old replica to stop in words.
HISTORY=(
    "create collection notes (title text nocase @hash @text, kind text @hash, n int @sorted, meta json)"
    "create collection docs (v vector<3> @hnsw(cosine), label text @text)"
    "create collection ledger (o int)"
    "put notes [{title: 'Hello', kind: 'open', n: 1, meta: {a: [1, 2], b: 'x'}}, {title: 'two', kind: 'open', n: 2}, {title: 'three', kind: 'void', n: 3}]"
    "put docs [{v: [1, 0, 0.5], label: 'first'}, {v: [0, 1, 0], label: 'second'}, {v: [0.25, 0.25, 1], label: 'third'}]"
    "create statement titles() as get notes select title order n"
    "create statement paid after set notes where new.kind = 'paid' as insert ledger {o: new.id}"
    "create statement tidy every 1d as del notes where kind = 'gone'"
    "create statement two() public as begin insert ledger {o: 100}; insert ledger {o: 101} end"
    "create statement pushed(\$sha text from event.after) on hook gh where hook.event = 'push' as insert ledger {o: 7}"
    "call two"
    "set notes {kind: 'paid'} where title = 'HELLO'"
    "del notes where n = 3"
    "alter collection notes add field extra text"
    "create collection scratch (x int)"
    "drop collection scratch"
    "put notes {title: 'after', kind: 'open', n: 4, extra: 'x'}"
    "set docs {label: 'moved'} where label = 'second'"
    "put notes {title: 'last', kind: 'open', n: 5}"
)
UNKNOWN_TO_OLD=()
# Where a replica started after the history began takes its image.
HALF=10
READS=(
    "get notes order id"
    "get docs select id, v, label order id"
    "get ledger order id"
    "call titles"
)

# Every read of READS at URL, one normalized answer a line; a refusal is
# `"error"`, whatever its words, so states of two versions compare.
state() {
    local r
    for r in "${READS[@]}"; do
        q "$1" "$r" "${2:-t}" |
            jq -S -c 'if type == "object" and has("error") then "error" else . end' 2>/dev/null ||
            echo "no answer to $r"
    done
}

# until_state URL WANT [SECONDS] [TOKEN]: waits for URL's state to be WANT.
until_state() {
    local tries=$((${3:-60} * 10))
    for _ in $(seq 1 "$tries"); do
        [ "$(state "$1" "${4:-t}")" = "$2" ] && return 0
        sleep 0.1
    done
    return 1
}

# A line a binary said, without the paths this run made.
bare() { sed "s|$C/||g; s|$WORK/||g"; }

# The first line of two states that differs, each side named.
first_difference() {
    diff <(echo "$1") <(echo "$2") | grep '^[<>]' | head -2 | cut -c1-300 | tr '\n' ' '
}

# history URL FROM TO: HISTORY's writes FROM to TO (exclusive), a snapshot of
# URL's state after each kept in SNAP[i+1].
history() {
    local i
    for ((i = $2; i < $3; i++)); do
        w "$1" "${HISTORY[$i]}"
        SNAP[i + 1]=$(state "$1")
    done
}
SNAP=()

# opened NAME BIN_DIR FILE: FILE opened by BIN_DIR's server, its state in
# OPENED; a replica's file is opened as a copy, promoted.
opened() {
    local name=$1 bin=$2 file=$3
    shift 3
    server "$name" "$bin" --file "$file" --http-token t "$@"
    OPENED=$(state "$ADDR")
    stop "$PID"
}

# copy_of FILE: a copy beside it, taken with the file closed.
copy_of() {
    cp "$1" "$1.copy.fennec"
    echo "$1.copy.fennec"
}

# --- a: replication ------------------------------------------------------------

case_a1() {
    server primary "$OLD" --file "$C/p.fennec" --http-token t --replication-token r
    local P=$ADDR
    history "$P" 0 "$HALF"
    server replica "$NEW" --file "$C/r.fennec" --http-token t --replication-token r --replica-of "$P"
    local R=$ADDR RPID=$PID
    history "$P" "$HALF" "${#HISTORY[@]}"
    local want=${SNAP[${#HISTORY[@]}]}
    if ! until_state "$R" "$want"; then
        fail "the new replica's reads are not the old primary's: $(first_difference "$want" "$(state "$R")"); its log: $(grep -m1 -i 'could not\|refus' "$C/replica.log")"
    fi
    case $want in
    *'"error"'*) fail "the old primary refused a read of its own history: $want" ;;
    esac
    stop "$RPID"
    opened reopened "$NEW" "$(copy_of "$C/r.fennec")" --promote
    [ "$OPENED" = "$want" ] ||
        fail "the new replica's file, opened again, is not what it answered: $(first_difference "$want" "$OPENED")"
    held a1 "the new replica followed the old primary's ${#HISTORY[@]} writes, $HALF in its image and the rest as records; its reads and its file opened again are the primary's"
}

case_a2() {
    server primary "$NEW" --file "$C/p.fennec" --http-token t --replication-token r
    local P=$ADDR
    SNAP[0]=$(state "$P")
    history "$P" 0 "$HALF"
    server replica "$OLD" --file "$C/r.fennec" --http-token t --replication-token r --replica-of "$P"
    local R=$ADDR RPID=$PID
    history "$P" "$HALF" "${#HISTORY[@]}"
    local n=${#HISTORY[@]} got k i stopped=
    until_state "$R" "${SNAP[$n]}" 30 || true
    got=$(state "$R")
    k=
    for ((i = n; i >= 0; i--)); do
        if [ "${SNAP[$i]}" = "$got" ]; then
            k=$i
            break
        fi
    done
    [ -n "$k" ] ||
        fail "the old replica holds what no prefix of the primary's history holds: $(first_difference "${SNAP[$n]}" "$got")"
    if [ "$k" -lt "$n" ]; then
        stopped=$(grep -m1 'could not apply' "$C/replica.log" || true)
        local unknown=
        for i in "${UNKNOWN_TO_OLD[@]:-}"; do [ "$i" = "$k" ] && unknown=1; done
        if [ -z "$unknown" ]; then
            fail "the old replica stopped before write $((k + 1)), \`${HISTORY[$k]}\`, which $OLD_SAYS reads: ${stopped:-it said nothing}"
        fi
        echo "$stopped" | grep -Eq 'record kind [0-9]+|type tag [0-9]+|version|does not read' ||
            fail "the old replica stopped before \`${HISTORY[$k]}\` without naming what it does not know: ${stopped:-it said nothing}"
    fi
    stop "$RPID"
    opened reopened "$OLD" "$(copy_of "$C/r.fennec")" --promote
    [ "$OPENED" = "$got" ] ||
        fail "the old replica's file, opened by $OLD_SAYS, is not what it answered: $(first_difference "$got" "$OPENED")"
    if [ "$k" -lt "$n" ]; then
        held a2 "the old replica stopped in words before write $((k + 1)) (${stopped}); its file, opened by the old binary, holds the history's first $k writes"
    else
        held a2 "the old replica followed all ${n} writes of the new primary (no record kind, tag or op is new since $OLD_SAYS); its file, opened by the old binary, holds them all"
    fi
}

case_a3() {
    local deep='1' i
    for ((i = 0; i < 63; i++)); do deep="[$deep]"; done
    server primary "$OLD" --file "$C/p.fennec" --http-token t --replication-token r
    local P=$ADDR PPID_=$PID
    w "$P" "create collection deep (v [[json]])"
    w "$P" "put deep {v: [[$deep]]}"
    local want
    want=$(q "$P" "get deep count")
    server replica "$NEW" --file "$C/r.fennec" --http-token t --replication-token r --replica-of "$P"
    local R=$ADDR
    w "$P" "put deep {v: [[1]]}"
    want=$(q "$P" "get deep select id order id")
    local got=
    for _ in $(seq 1 300); do
        got=$(q "$R" "get deep select id order id")
        [ "$got" = "$want" ] && break
        grep -q 'could not apply' "$C/replica.log" && break
        sleep 0.1
    done
    if [ "$got" = "$want" ] && [ "$(q "$R" "get deep order id" | jq -c length)" = 2 ]; then
        held a3 "a value nested 66 levels deep, which $OLD_SAYS writes, followed and read by the new replica"
        return
    fi
    local old_read new_read new_open
    old_read=$(q "$P" "get deep select id order id")
    new_read=$(q "$R" "get deep order id" | cut -c1-200)
    stop "$PPID_"
    server reopen "$NEW" --file "$C/p.fennec" --http-token t
    new_open=$(q "$ADDR" "get deep order id" | cut -c1-200)
    differs a3 "an old primary's \`[[json]]\` value nested 66 levels deep (\`put deep {v: [[<64 levels>]]}\`, which $OLD_SAYS takes): the old primary answers \`get deep select id order id\` with $old_read; the new replica logs \`$(grep -m1 'could not apply' "$C/replica.log" | sed 's/^ *//')\` and answers \`get deep order id\` with $new_read; the new server opening the old primary's file answers it with $new_open"
}

# What the old release writes and this tree no longer takes, but reads: a
# stored definition holding a number past a float's range (this tree's
# parser refuses `1e400` in a new text, and reads a stored one as it did,
# to its digest) and a vector's component past an f32's (`1e39`, refused by
# this tree's writes, read as the file holds it).
case_a4() {
    server primary "$OLD" --file "$C/p.fennec" --http-token t --replication-token r
    local P=$ADDR
    local -a READS
    READS=(
        "get far order id"
        "call below"
    )
    w "$P" "create collection far (n float, v vector<3>)"
    w "$P" "put far [{n: 1, v: [1, 0, 0]}, {n: 2, v: [1e39, 0, 0]}]"
    w "$P" "create statement below() as get far select n where n < 1e400 order n"
    server replica "$NEW" --file "$C/r.fennec" --http-token t --replication-token r --replica-of "$P"
    local R=$ADDR want broken
    want=$(state "$P")
    if until_state "$R" "$want" 30; then
        # The listing's other fields may grow between releases; whether the
        # definition is marked broken may not.
        broken=$(q "$R" statements | jq -c '[.[] | select(.name == "below") | .broken]')
        [ "$broken" = '[null]' ] || {
            differs a4 "the new replica marks the old primary's definition holding 1e400 broken: $broken"
            return
        }
        held a4 "a definition holding 1e400 and a vector component of 1e39, which $OLD_SAYS takes and this tree refuses to write, followed by the new replica, read and called there as the old primary reads and calls them"
    else
        differs a4 "what $OLD_SAYS writes and this tree refuses (a definition holding 1e400, a vector component of 1e39), on the new replica: $(first_difference "$want" "$(state "$R")")"
    fi
}

# --- b: archives ----------------------------------------------------------------

# archived ARCHIVE BIN_DIR OUT [ARGS...]: BIN_DIR's `fennec restore` of
# ARCHIVE into OUT, opened by BIN_DIR's server; its state in OPENED.
archived() {
    local archive=$1 bin=$2 out=$3
    shift 3
    "$bin/fennec" restore "$archive" "$out" "$@" >"$C/$(basename "$out").restore.log" 2>&1 || {
        OPENED="the restore failed: $(tr '\n' ' ' <"$C/$(basename "$out").restore.log")"
        return 0
    }
    opened "$(basename "$out" .fennec)" "$bin" "$out"
}

# The magic a sealed file starts with, its version byte as a digit.
sealed_version() {
    head -c 8 "$1" | od -An -c | tr -d ' \n' | sed -n 's/^FENECSL00\([0-9]\)$/\1/p'
}

case_b() {
    server primary "$OLD" --file "$C/p.fennec" --http-token t --replication-token r --sync always
    local P=$ADDR
    "$OLD/fennec" key "$C/old.key" >"$C/old-key.log" 2>&1 || fail "the old \`fennec key\` made no key"
    "$NEW/fennec" key "$C/new.key" >"$C/new-key.log" 2>&1 || fail "the new \`fennec key\` made no key"
    local a
    spawn "$C/old-plain.log" "$OLD/fennec" archive "$P" "$C/old-plain" --token r --image-every 1s
    local A1=$SPAWNED
    spawn "$C/old-sealed.log" "$OLD/fennec" archive "$P" "$C/old-sealed" --token r --image-every 1s \
        --key-file "$C/old.key"
    local A2=$SPAWNED
    spawn "$C/new-plain.log" "$NEW/fennec" archive "$P" "$C/new-plain" --token r --image-every 1s
    local A3=$SPAWNED
    spawn "$C/new-sealed.log" "$NEW/fennec" archive "$P" "$C/new-sealed" --token r --image-every 1s \
        --key-file "$C/new.key"
    local A4=$SPAWNED
    for a in old-plain old-sealed new-plain new-sealed; do
        said "$C/$a.log" 'image at change' || fail "the $a archiver took no image: $(tail -2 "$C/$a.log")"
    done
    history "$P" 0 "$HALF"
    # A moment between two halves, a second and more from either.
    sleep 1.5
    local at moment_state
    at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    moment_state=${SNAP[$HALF]}
    sleep 1.5
    history "$P" "$HALF" "${#HISTORY[@]}"
    local want=${SNAP[${#HISTORY[@]}]} seq
    seq=$(curl -sS -H 'Authorization: Bearer r' "$P/_replication/status" | jq -r .seq)
    for a in old-plain new-plain; do
        for _ in $(seq 1 600); do
            "$NEW/fennec" verify "$C/$a" 2>&1 | grep -q "to $seq," && break
            sleep 0.05
        done
    done
    for a in old-sealed new-sealed; do
        local key=$C/old.key
        [ "$a" = new-sealed ] && key=$C/new.key
        for _ in $(seq 1 600); do
            "$NEW/fennec" verify "$C/$a" --key-file "$key" 2>&1 | grep -q "to $seq," && break
            sleep 0.05
        done
    done
    stop "$A1" "$A2" "$A3" "$A4"

    # b1: the old archiver's plain archive, read by the new `fennec`.
    local why=
    "$NEW/fennec" verify "$C/old-plain" >"$C/b1-verify.log" 2>&1 ||
        why="the new verify refused it: $(tr '\n' ' ' <"$C/b1-verify.log")"
    if [ -z "$why" ]; then
        archived "$C/old-plain" "$NEW" "$C/b1-end.fennec"
        [ "$OPENED" = "$want" ] || why="restored to its end: $(first_difference "$want" "$OPENED")"
    fi
    if [ -z "$why" ]; then
        archived "$C/old-plain" "$NEW" "$C/b1-at.fennec" --to "$at"
        [ "$OPENED" = "$moment_state" ] || why="restored to $at: $(first_difference "$moment_state" "$OPENED")"
    fi
    if [ -z "$why" ]; then
        held b1 "the old archiver's archive verified by the new \`fennec\`, restored to its end (change $seq) and to a moment between two writes"
    else
        differs b1 "the old archiver's archive, read by the new \`fennec\`: $why"
    fi

    # b2: the old archiver's sealed archive (version 1), read by the new.
    why=
    local v
    v=$(sealed_version "$(find "$C/old-sealed" -name 'image-*' | head -1)")
    [ "$v" = 1 ] || why="its image is no version 1 sealed file (version '$v')"
    if [ -z "$why" ]; then
        "$NEW/fennec" verify "$C/old-sealed" --key-file "$C/old.key" >"$C/b2-verify.log" 2>&1 ||
            why="the new verify refused it: $(tr '\n' ' ' <"$C/b2-verify.log")"
    fi
    if [ -z "$why" ]; then
        archived "$C/old-sealed" "$NEW" "$C/b2-end.fennec" --key-file "$C/old.key"
        [ "$OPENED" = "$want" ] || why="restored to its end: $(first_difference "$want" "$OPENED")"
    fi
    if [ -z "$why" ]; then
        archived "$C/old-sealed" "$NEW" "$C/b2-at.fennec" --key-file "$C/old.key" --to "$at"
        [ "$OPENED" = "$moment_state" ] || why="restored to $at: $(first_difference "$moment_state" "$OPENED")"
    fi
    if [ -z "$why" ]; then
        held b2 "the old archiver's sealed archive (version 1) verified and restored by the new \`fennec\`, to its end and to a moment"
    else
        differs b2 "the old archiver's sealed archive, read by the new \`fennec\`: $why"
    fi

    # b3: the new archiver's plain archive, read by the old `fennec`: no
    # format moved since the release, so it reads.
    why=
    "$OLD/fennec" verify "$C/new-plain" >"$C/b3-verify.log" 2>&1 ||
        why="the old verify refused it: $(tr '\n' ' ' <"$C/b3-verify.log")"
    if [ -z "$why" ]; then
        archived "$C/new-plain" "$OLD" "$C/b3-end.fennec"
        [ "$OPENED" = "$want" ] || why="restored to its end: $(first_difference "$want" "$OPENED")"
    fi
    if [ -z "$why" ]; then
        archived "$C/new-plain" "$OLD" "$C/b3-at.fennec" --to "$at"
        [ "$OPENED" = "$moment_state" ] || why="restored to $at: $(first_difference "$moment_state" "$OPENED")"
    fi
    if [ -z "$why" ]; then
        held b3 "the new archiver's plain archive verified and restored by the old \`fennec\`, to its end and to a moment"
    else
        differs b3 "the new archiver's plain archive, read by the old \`fennec\`: $why"
    fi

    # b4: the new archiver's sealed archive is version 2, which the old
    # `fennec` refuses as RELEASING.md says ("not a sealed file", "not sealed,
    # where the archive is"), writing nothing; the new reads it.
    why=
    v=$(sealed_version "$(find "$C/new-sealed" -name 'image-*' | head -1)")
    [ "$v" = 2 ] || why="its image is no version 2 sealed file (version '$v')"
    local verify_said restore_said
    if [ -z "$why" ]; then
        if "$OLD/fennec" verify "$C/new-sealed" --key-file "$C/new.key" >"$C/b4-verify.log" 2>&1; then
            why="the old verify read it"
        elif ! grep -q 'not sealed, where the archive is' "$C/b4-verify.log"; then
            why="the old verify refused it in other words: $(tr '\n' ' ' <"$C/b4-verify.log")"
        fi
        verify_said=$(grep -m1 'not sealed' "$C/b4-verify.log" | bare || true)
    fi
    if [ -z "$why" ]; then
        if "$OLD/fennec" restore "$C/new-sealed" "$C/b4-old.fennec" --key-file "$C/new.key" \
            >"$C/b4-restore.log" 2>&1; then
            why="the old restore read it"
        elif ! grep -Eq 'not (a )?sealed' "$C/b4-restore.log"; then
            why="the old restore refused it in other words: $(tr '\n' ' ' <"$C/b4-restore.log")"
        elif [ -e "$C/b4-old.fennec" ]; then
            why="the old restore wrote a file"
        fi
        restore_said=$(grep -Em1 'not (a )?sealed' "$C/b4-restore.log" | bare || true)
    fi
    if [ -z "$why" ]; then
        archived "$C/new-sealed" "$NEW" "$C/b4-new.fennec" --key-file "$C/new.key"
        [ "$OPENED" = "$want" ] || why="the new restore of it: $(first_difference "$want" "$OPENED")"
    fi
    if [ -z "$why" ]; then
        held b4 "the new archiver's sealed archive (version 2) refused by the old \`fennec\` as promised -- verify: \"$verify_said\"; restore: \"$restore_said\", no file written -- and restored by the new"
    else
        differs b4 "the new archiver's sealed archive: $why"
    fi
}

# --- c: backups and files -------------------------------------------------------

case_c() {
    server primary "$OLD" --file "$C/p.fennec" --http-token t --replication-token r
    local P=$ADDR PP=$PID
    history "$P" 0 "${#HISTORY[@]}"
    local want=${SNAP[${#HISTORY[@]}]} why= v
    "$OLD/fennec" key "$C/old.key" >"$C/old-key.log" 2>&1 || fail "the old \`fennec key\` made no key"

    # c1: the old `fennec backup`, plain and sealed, opened by the new.
    "$OLD/fennec" backup "$P" "$C/backup.fennec" --token r >"$C/c1-backup.log" 2>&1 ||
        fail "the old backup failed: $(tr '\n' ' ' <"$C/c1-backup.log")"
    "$OLD/fennec" backup "$P" "$C/backup.sealed" --token r --key-file "$C/old.key" \
        >"$C/c1-sealed.log" 2>&1 || fail "the old sealed backup failed: $(tr '\n' ' ' <"$C/c1-sealed.log")"
    opened c1-open "$NEW" "$C/backup.fennec"
    [ "$OPENED" = "$want" ] || why="its plain backup, opened by the new server: $(first_difference "$want" "$OPENED")"
    v=$(sealed_version "$C/backup.sealed")
    [ -n "$why" ] || [ "$v" = 1 ] || why="its sealed backup is no version 1 file (version '$v')"
    if [ -z "$why" ]; then
        archived "$C/backup.sealed" "$NEW" "$C/c1-sealed.fennec" --key-file "$C/old.key"
        [ "$OPENED" = "$want" ] || why="its sealed backup, restored by the new \`fennec\`: $(first_difference "$want" "$OPENED")"
    fi
    if [ -z "$why" ]; then
        held c1 "the old \`fennec backup\`, plain and sealed (version 1), opened by the new server and restored by the new \`fennec\`"
    else
        differs c1 "the old \`fennec backup\`: $why"
    fi

    # c2: the old server's file, opened and written by the new, then opened
    # by the old: no feature since the release moves the file, so the
    # rollback compatibility.md promises before a feature's first use holds.
    why=
    stop "$PP"
    server new-on-old "$NEW" --file "$C/p.fennec" --http-token t
    local N=$ADDR NP=$PID after
    [ "$(state "$N")" = "$want" ] || why="the new server's reads of it: $(first_difference "$want" "$(state "$N")")"
    w "$N" "put notes {title: 'by the new release', kind: 'open', n: 9}"
    w "$N" "set docs {label: 'set by the new release'} where label = 'first'"
    after=$(state "$N")
    stop "$NP"
    if [ -z "$why" ]; then
        "$NEW/fennec" verify "$C/p.fennec" >"$C/c2-verify-new.log" 2>&1 ||
            why="the new verify refused it: $(tr '\n' ' ' <"$C/c2-verify-new.log")"
    fi
    if [ -z "$why" ]; then
        "$OLD/fennec" verify "$C/p.fennec" >"$C/c2-verify-old.log" 2>&1 ||
            why="the old verify refused it: $(tr '\n' ' ' <"$C/c2-verify-old.log")"
    fi
    if [ -z "$why" ]; then
        "$NEW/fennec" types "$C/p.fennec" >"$C/c2-types-new.out" 2>"$C/c2-types-new.log" ||
            why="the new \`fennec types\` refused it: $(tr '\n' ' ' <"$C/c2-types-new.log")"
    fi
    if [ -z "$why" ]; then
        "$OLD/fennec" types "$C/p.fennec" >"$C/c2-types-old.out" 2>"$C/c2-types-old.log" ||
            why="the old \`fennec types\` refused it: $(tr '\n' ' ' <"$C/c2-types-old.log")"
    fi
    if [ -z "$why" ]; then
        opened old-again "$OLD" "$C/p.fennec"
        [ "$OPENED" = "$after" ] || why="opened by the old server again: $(first_difference "$after" "$OPENED")"
    fi
    if [ -z "$why" ]; then
        held c2 "a file the old server wrote, opened and written by the new, verified and typed (\`fennec types\`) by both and opened by the old again with the new writes"
    else
        differs c2 "a file the old server wrote, then the new: $why"
    fi

    # c3: the new `fennec backup`, plain, opened by the old server; sealed
    # (version 2), refused by the old `fennec restore` in RELEASING.md's words.
    why=
    server new-primary "$NEW" --file "$C/n.fennec" --http-token t --replication-token r
    local NN=$ADDR
    SNAP=()
    history "$NN" 0 "${#HISTORY[@]}"
    want=${SNAP[${#HISTORY[@]}]}
    "$NEW/fennec" key "$C/new.key" >"$C/new-key.log" 2>&1 || fail "the new \`fennec key\` made no key"
    "$NEW/fennec" backup "$NN" "$C/new-backup.fennec" --token r >"$C/c3-backup.log" 2>&1 ||
        fail "the new backup failed: $(tr '\n' ' ' <"$C/c3-backup.log")"
    "$NEW/fennec" backup "$NN" "$C/new-backup.sealed" --token r --key-file "$C/new.key" \
        >"$C/c3-sealed.log" 2>&1 || fail "the new sealed backup failed: $(tr '\n' ' ' <"$C/c3-sealed.log")"
    opened c3-open "$OLD" "$C/new-backup.fennec"
    [ "$OPENED" = "$want" ] || why="its plain backup, opened by the old server: $(first_difference "$want" "$OPENED")"
    v=$(sealed_version "$C/new-backup.sealed")
    [ -n "$why" ] || [ "$v" = 2 ] || why="its sealed backup is no version 2 file (version '$v')"
    local said_=
    if [ -z "$why" ]; then
        if "$OLD/fennec" restore "$C/new-backup.sealed" "$C/c3-old.fennec" --key-file "$C/new.key" \
            >"$C/c3-restore.log" 2>&1; then
            why="the old restore read its sealed backup"
        elif ! grep -q 'not a sealed file' "$C/c3-restore.log"; then
            why="the old restore refused its sealed backup in other words: $(tr '\n' ' ' <"$C/c3-restore.log")"
        elif [ -e "$C/c3-old.fennec" ]; then
            why="the old restore wrote a file from its sealed backup"
        fi
        said_=$(grep -m1 'not a sealed file' "$C/c3-restore.log" | bare || true)
    fi
    if [ -z "$why" ]; then
        held c3 "the new \`fennec backup\` opened by the old server; sealed (version 2), refused by the old restore as promised: \"$said_\", no file written"
    else
        differs c3 "the new \`fennec backup\`: $why"
    fi
}

# --- d: the router ----------------------------------------------------------------

# The address node N said it listens on.
node_at() { cat "$C/$1.addr"; }

# router_case NAME ROUTER_BIN NODE_BIN: a tenant created by ROUTER_BIN's
# router on NODE_BIN's nodes, written, copied to its replica, moved and
# deleted, each step working or refused in words.
router_case() {
    local name=$1 rbin=$2 nbin=$3 n code
    for n in n1 n2 n3; do
        server "$n" "$nbin" --dir "$C/$n" --admin-token "a$n" --replication-token r --sync always
        echo "$ADDR" >"$C/$n.addr"
    done
    router router "$rbin" --directory "$C/router.fennec" --token root --replicas
    local RT=$ADDR
    for n in n1 n2 n3; do
        code=$(admin "$RT" PUT "/_shard/nodes/$n" "{\"addr\":\"$(node_at "$n" | sed "s|^http://||")\",\"token\":\"a$n\"}")
        case $code in
        2??) ;;
        *) fail "the router took no node $n ($code): $(cat "$C/admin.out")" ;;
        esac
    done
    code=$(admin "$RT" PUT /_shard/tenants/acme '{"node":"n1"}')
    case $code in
    2??) ;;
    *) fail "the create was refused ($code): $(cat "$C/admin.out")" ;;
    esac
    history "$RT/t/acme" 0 "${#HISTORY[@]}"
    local want=${SNAP[${#HISTORY[@]}]} rep
    admin "$RT" GET /_shard/tenants/acme >/dev/null
    rep=$(jq -r '.replica // .replica_on // empty' "$C/admin.out")
    [ -n "$rep" ] || fail "the tenant was given no replica: $(cat "$C/admin.out")"
    until_state "$(node_at "$rep")/t/acme" "$want" ||
        fail "the replica on $rep is not the tenant: $(first_difference "$want" "$(state "$(node_at "$rep")/t/acme")")"
    local to=
    for n in n2 n3; do [ "$n" != "$rep" ] && to=$n; done
    code=$(admin "$RT" POST /_shard/tenants/acme/move "{\"to\":\"$to\"}")
    case $code in
    2??) ;;
    *) fail "the move to $to was refused ($code): $(cat "$C/admin.out")" ;;
    esac
    admin "$RT" GET /_shard/tenants/acme >/dev/null
    jq -e --arg to "$to" '.node == $to' "$C/admin.out" >/dev/null ||
        fail "the tenant is not on $to after its move: $(cat "$C/admin.out")"
    [ "$(state "$RT/t/acme")" = "$want" ] ||
        fail "the moved tenant's reads: $(first_difference "$want" "$(state "$RT/t/acme")")"
    w "$RT/t/acme" "put notes {title: 'moved', kind: 'open', n: 6}"
    want=$(state "$RT/t/acme")
    admin "$RT" GET /_shard/tenants/acme >/dev/null
    rep=$(jq -r '.replica // .replica_on // empty' "$C/admin.out")
    [ -n "$rep" ] || fail "the moved tenant has no replica: $(cat "$C/admin.out")"
    until_state "$(node_at "$rep")/t/acme" "$want" ||
        fail "the moved tenant's replica on $rep is not the tenant: $(first_difference "$want" "$(state "$(node_at "$rep")/t/acme")")"
    code=$(admin "$RT" DELETE /_shard/tenants/acme)
    [ "$code" = 204 ] || fail "the delete answered $code: $(cat "$C/admin.out")"
    for n in n1 n2 n3; do
        [ -e "$C/$n/acme.fennec" ] && fail "the delete left $n's acme.fennec"
    done
    held "$name" "a tenant created on a node with a replica on another, its history written through the router, copied, moved to a third node with its replica following, and deleted from every node"
}

case_d1() { router_case d1 "$OLD" "$NEW"; }
case_d2() { router_case d2 "$NEW" "$OLD"; }

# --- e: a rolling upgrade under writes ---------------------------------------------

# restart NAME BIN_DIR PORT ARGS...: a node or a router stopped, then started
# again by BIN_DIR on the port it was first given.
WRITES=3000

case_e() {
    local n1 n2 rt n1p n2p rtp p
    server n1 "$OLD" --dir "$C/n1" --admin-token a1 --replication-token r --sync always
    n1=$ADDR n1p=$PID
    server n2 "$OLD" --dir "$C/n2" --admin-token a2 --replication-token r --replica-of "$n1"
    n2=$ADDR n2p=$PID
    router router "$OLD" --directory "$C/router.fennec" --token root
    rt=$ADDR rtp=$PID
    local code
    code=$(admin "$rt" PUT /_shard/nodes/n2 "{\"addr\":\"${n2#http://}\",\"token\":\"a2\"}")
    case $code in 2??) ;; *) fail "the router took no node n2 ($code): $(cat "$C/admin.out")" ;; esac
    code=$(admin "$rt" PUT /_shard/nodes/n1 "{\"addr\":\"${n1#http://}\",\"token\":\"a1\",\"standby\":\"n2\"}")
    case $code in 2??) ;; *) fail "the router took no node n1 ($code): $(cat "$C/admin.out")" ;; esac
    code=$(admin "$rt" PUT /_shard/tenants/acme '{"node":"n1"}')
    case $code in 2??) ;; *) fail "the create was refused ($code): $(cat "$C/admin.out")" ;; esac
    w "$rt/t/acme" "create collection seqs (seq int @hash)"
    echo "$rt" >"$C/router.addr"

    # One writer, a fixed number of writes, each answered or not: an answer
    # with no error is acknowledged, durable (--sync always) on the primary.
    (
        # Its refusals are counted, not the case's end.
        trap - ERR
        set +eE
        trap 'exit 0' TERM
        for ((i = 1; i <= WRITES; i++)); do
            [ -e "$C/writer.stop" ] && break
            body=$(curl -sS --max-time 10 -H 'Authorization: Bearer t' -H 'Content-Type: application/json' \
                -d "{\"query\":\"put seqs {seq: $i}\"}" "$(cat "$C/router.addr")/t/acme/query" 2>&1) || true
            case $body in
            *'"affected":1'*) echo "$i ok" ;;
            *) echo "$i no ${body:0:120}" ;;
            esac
        done >"$C/writer.out"
        echo done >>"$C/writer.out"
    ) &
    local writer=$!
    echo "$writer" >>"$WORK/pids"
    echo "$writer" >>"$C/pids"
    acked() { grep -c ' ok$' "$C/writer.out" 2>/dev/null || true; }
    # Writes acknowledged before each step, so each step meets writes.
    wait_acked() {
        local before
        before=$(acked)
        for _ in $(seq 1 600); do
            [ "$(acked)" -ge $((before + $1)) ] && return 0
            kill -0 "$writer" 2>/dev/null || return 0
            sleep 0.05
        done
        fail "no write was acknowledged for 30 s: $(tail -2 "$C/writer.out")"
    }
    wait_acked 100

    # 1. Archivers: none here. 2. The router, restarted in place.
    p=${rt##*:}
    stop "$rtp"
    spawn "$C/router-new.log" "$NEW/fennec-shard" --listen "127.0.0.1:$p" --directory "$C/router.fennec" --token root
    rtp=$SPAWNED
    rt=$(addr "$C/router-new.log" "$rtp") || exit 1
    wait_acked 100

    # 3. The nodes, one at a time, the standby's first. The next waits for
    # the standby to hold every write the primary acknowledged (the
    # runbook's "FennecTenantsWithoutReplica to clear" for a pair).
    p=${n2##*:}
    stop "$n2p"
    spawn "$C/n2-new.log" "$NEW/fennec-server" --http "127.0.0.1:$p" --dir "$C/n2" --admin-token a2 \
        --replication-token r --replica-of "$n1"
    n2p=$SPAWNED
    addr "$C/n2-new.log" "$n2p" >/dev/null || exit 1
    wait_acked 100
    local held_by_n1
    held_by_n1=$(q "$n1/t/acme" "get seqs count" | jq -r '.[0].count')
    for _ in $(seq 1 600); do
        [ "$(q "$n2/t/acme" "get seqs count" | jq -r '.[0].count // 0' 2>/dev/null)" -ge "$held_by_n1" ] && break
        sleep 0.05
    done
    [ "$(q "$n2/t/acme" "get seqs count" | jq -r '.[0].count // 0' 2>/dev/null)" -ge "$held_by_n1" ] ||
        fail "the upgraded standby did not catch up: $(q "$n2/t/acme" "get seqs count") of $held_by_n1; $(tail -2 "$C/n2-new.log")"

    p=${n1##*:}
    stop "$n1p"
    spawn "$C/n1-new.log" "$NEW/fennec-server" --http "127.0.0.1:$p" --dir "$C/n1" --admin-token a1 \
        --replication-token r --sync always
    n1p=$SPAWNED
    addr "$C/n1-new.log" "$n1p" >/dev/null || exit 1
    wait_acked 100
    touch "$C/writer.stop"
    for _ in $(seq 1 600); do
        kill -0 "$writer" 2>/dev/null || break
        sleep 0.05
    done

    # Every acknowledged write is on the primary, read through the router,
    # and on the standby.
    local sent ok refused have lost lost_standby
    sent=$(grep -c '^[0-9]' "$C/writer.out" || true)
    ok=$(acked)
    refused=$((sent - ok))
    q "$rt/t/acme" "get seqs select seq" | jq -r '.[].seq' | sort >"$C/primary.seqs"
    grep ' ok$' "$C/writer.out" | cut -d' ' -f1 | sort >"$C/acked.seqs"
    lost=$(comm -23 "$C/acked.seqs" "$C/primary.seqs" | wc -l | tr -d ' ')
    have=$(wc -l <"$C/primary.seqs" | tr -d ' ')
    for _ in $(seq 1 600); do
        [ "$(q "$n2/t/acme" "get seqs count" | jq -r '.[0].count // 0' 2>/dev/null)" -ge "$have" ] && break
        sleep 0.05
    done
    q "$n2/t/acme" "get seqs select seq" | jq -r '.[].seq' | sort >"$C/standby.seqs"
    lost_standby=$(comm -23 "$C/acked.seqs" "$C/standby.seqs" | wc -l | tr -d ' ')
    [ "$ok" -gt 0 ] || fail "no write was acknowledged"
    [ "$lost" = 0 ] ||
        fail "$lost of $ok acknowledged writes are not on the primary: $(comm -23 "$C/acked.seqs" "$C/primary.seqs" | head -5 | tr '\n' ' ')"
    [ "$lost_standby" = 0 ] ||
        fail "$lost_standby of $ok acknowledged writes are not on the standby: $(comm -23 "$C/acked.seqs" "$C/standby.seqs" | head -5 | tr '\n' ' ')"
    held e "the router, the standby's node and the primary's node upgraded in turn under one writer: $sent writes sent, $ok acknowledged, $refused not ($((have - ok)) of them landed), 0 acknowledged lost on the primary or the standby"
}

# --- f: a GitHub hook kept with `key delivery` ---------------------------------------

# RELEASING.md: a single server whose hooks file has a `github` line saying
# `key delivery` refuses to start on the release, exit 2, naming the line;
# a tenant's `github` hook 0.2.0 saved with `key delivery` is kept, its
# events answered 409 until it is set again with `key body`, its statements
# not armed, and the node says so as it starts.
GH_SECRET=0123456789abcdef0123456789abcdef0123456789abcdef

# event URL BODY DELIVERY: a push GitHub signed; "<body> <status>".
event() {
    local sig
    sig=$(printf '%s' "$2" | openssl dgst -sha256 -hmac "$GH_SECRET" | sed 's/^.*= //')
    curl -sS --max-time 60 -w ' %{http_code}' -H 'Content-Type: application/json' \
        -H 'X-GitHub-Event: push' -H "X-GitHub-Delivery: $3" \
        -H "X-Hub-Signature-256: sha256=$sig" -d "$2" "$1"
}

case_f() {
    # f1: the single server's hooks file.
    printf '%s' "$GH_SECRET" >"$C/gh.secret"
    chmod 600 "$C/gh.secret"
    echo "gh github $C/gh.secret key delivery" >"$C/hooks"
    server old-single "$OLD" --file "$C/s.fennec" --http-token t --hooks "$C/hooks"
    stop "$PID"
    local rc=0 said_
    "$NEW/fennec-server" --file "$C/s.fennec" --http 127.0.0.1:0 --http-token t --hooks "$C/hooks" \
        >"$C/new-single.log" 2>&1 || rc=$?
    said_=$(grep -m1 'hooks file line 1: `key delivery` is refused' "$C/new-single.log" | bare | cut -c1-120 || true)
    if [ "$rc" = 2 ] && [ -n "$said_" ]; then
        held f1 "a hooks file's \`github\` line saying \`key delivery\`: $OLD_SAYS starts on it, the new server refuses it as promised, exit 2: \"$said_...\""
    else
        differs f1 "a hooks file's \`github\` line saying \`key delivery\`: the new server exited $rc, saying $(tr '\n' ' ' <"$C/new-single.log" | bare | cut -c1-300)"
    fi

    # f2: a tenant's hook, set on the old node; the router and then the node
    # upgraded in turn.
    server n1 "$OLD" --dir "$C/n1" --admin-token a1
    local n1=$ADDR n1p=$PID
    router router "$OLD" --directory "$C/router.fennec" --token root
    local rt=$ADDR rtp=$PID code out p
    code=$(admin "$rt" PUT /_shard/nodes/n1 "{\"addr\":\"${n1#http://}\",\"token\":\"a1\"}")
    case $code in 2??) ;; *) fail "the router took no node ($code): $(cat "$C/admin.out")" ;; esac
    code=$(admin "$rt" PUT /_shard/tenants/acme '{"node":"n1"}')
    case $code in 2??) ;; *) fail "the create was refused ($code): $(cat "$C/admin.out")" ;; esac
    code=$(admin "$rt" PUT /_shard/tenants/acme/hooks/gh \
        "{\"scheme\":\"github\",\"secrets\":[\"$GH_SECRET\"],\"key\":\"delivery\"}")
    case $code in 2??) ;; *) fail "the old router refused the hook ($code): $(cat "$C/admin.out")" ;; esac
    w "$rt/t/acme" "create collection pushes (sha text)"
    w "$rt/t/acme" "create statement pushed(\$sha text from event.after) on hook gh where hook.event = 'push' as insert pushes {sha: \$sha}"
    out=$(event "$rt/t/acme/_hooks/gh" '{"after":"aaa","ref":"refs/heads/main"}' d1)
    case $out in *' 200') ;; *) fail "the old node took no event: $out" ;; esac

    p=${rt##*:}
    stop "$rtp"
    spawn "$C/router-new.log" "$NEW/fennec-shard" --listen "127.0.0.1:$p" --directory "$C/router.fennec" --token root
    rtp=$SPAWNED
    rt=$(addr "$C/router-new.log" "$rtp") || exit 1
    p=${n1##*:}
    stop "$n1p"
    spawn "$C/n1-new.log" "$NEW/fennec-server" --http "127.0.0.1:$p" --dir "$C/n1" --admin-token a1
    n1p=$SPAWNED
    addr "$C/n1-new.log" "$n1p" >/dev/null || exit 1

    local why= refused listing armed
    refused=$(event "$rt/t/acme/_hooks/gh" '{"after":"bbb","ref":"refs/heads/main"}' d2)
    case $refused in
    *'set again with `key body`'*' 409') ;;
    *) why="its verified event was answered $refused" ;;
    esac
    [ -n "$why" ] || [ "$(q "$rt/t/acme" "get pushes count" | jq -r '.[0].count')" = 1 ] ||
        why="the refused event wrote: $(q "$rt/t/acme" "get pushes")"
    [ -n "$why" ] || grep -q 'was set with `key delivery`' "$C/n1-new.log" ||
        why="the upgraded node said nothing of the hook as it started: $(tail -3 "$C/n1-new.log" | tr '\n' ' ')"
    listing=$(curl -sS -H 'Authorization: Bearer root' "$rt/_shard/tenants/acme/hooks")
    [ -n "$why" ] || echo "$listing" | jq -e '.hooks[0].key == "delivery"' >/dev/null ||
        why="its listing does not say \"key\":\"delivery\": $listing"
    armed=$(q "$rt/t/acme" "statements" | jq -c '[.[] | select(.name == "pushed") | .armed]')
    [ -n "$why" ] || [ "$armed" = '[false]' ] || why="its statement's \`armed\` is $armed"
    if [ -z "$why" ]; then
        code=$(admin "$rt" PUT /_shard/tenants/acme/hooks/gh "{\"scheme\":\"github\",\"secrets\":[\"$GH_SECRET\"],\"key\":\"body\"}")
        case $code in 2??) ;; *) why="setting it again with \`key body\` was refused ($code): $(cat "$C/admin.out")" ;; esac
    fi
    if [ -z "$why" ]; then
        out=$(event "$rt/t/acme/_hooks/gh" '{"after":"bbb","ref":"refs/heads/main"}' d3)
        case $out in *' 200') ;; *) why="the redelivery after \`key body\` was answered $out" ;; esac
        [ -n "$why" ] || [ "$(q "$rt/t/acme" "get pushes count" | jq -r '.[0].count')" = 2 ] ||
            why="the redelivery wrote nothing: $(q "$rt/t/acme" "get pushes")"
    fi
    if [ -z "$why" ]; then
        held f2 "a tenant's \`github\` hook the old node kept with \`key delivery\`: after the router and the node were upgraded its verified event was refused as promised (409, \"$(echo "${refused% *}" | jq -r .error 2>/dev/null | cut -c1-60)...\"), nothing written, the node's log naming it, its listing \"key\":\"delivery\" and its statement not armed; set again with \`key body\`, the redelivery was taken"
    else
        differs f2 "a tenant's \`github\` hook kept with \`key delivery\`: $why"
    fi
}

# --- g: a node recorded at https:// ---------------------------------------------

# https_case NAME ROUTER_BIN NODE_BIN: a node serving HTTPS, recorded at its
# https:// address by a router of the other release, which dials it over TLS.
https_case() {
    local name=$1 rbin=$2 nbin=$3 certs=$ROOT/crates/fennec-http/tests/certs port code
    spawn "$C/node.log" "$nbin/fennec-server" --dir "$C/node" --http 127.0.0.1:0 --admin-token atls \
        --tls-cert "$certs/server.pem" --tls-key "$certs/server.key"
    local np=$SPAWNED
    for _ in $(seq 1 600); do
        grep -q 'listening on: https://' "$C/node.log" && break
        kill -0 "$np" 2>/dev/null || fail "the node serving HTTPS exited: $(tail -2 "$C/node.log")"
        sleep 0.05
    done
    port=$(grep -o 'listening on: https://[^ ]*' "$C/node.log" | head -1 | sed 's/.*://')
    [ -n "$port" ] || fail "the node serving HTTPS said no address"
    router router "$rbin" --directory "$C/router.fennec" --token root --tls-ca "$certs/ca.pem"
    local rt=$ADDR
    code=$(admin "$rt" PUT /_shard/nodes/t1 "{\"addr\":\"https://localhost:$port\",\"token\":\"atls\"}")
    case $code in 2??) ;; *) fail "the router took no https:// node ($code): $(cat "$C/admin.out")" ;; esac
    code=$(admin "$rt" PUT /_shard/tenants/secure '{"node":"t1"}')
    case $code in 2??) ;; *) fail "no tenant on the https:// node ($code): $(cat "$C/admin.out")" ;; esac
    history "$rt/t/secure" 0 "${#HISTORY[@]}"
    local want=${SNAP[${#HISTORY[@]}]}
    case $want in
    *'"error"'*) fail "a read through the router to the https:// node was refused: $want" ;;
    esac
    code=$(admin "$rt" DELETE /_shard/tenants/secure)
    [ "$code" = 204 ] || fail "the delete answered $code: $(cat "$C/admin.out")"
    held "$name" "a node serving HTTPS, recorded at https://, a tenant on it created, written, read and deleted through the router over TLS"
}

case_g1() { https_case g1 "$OLD" "$NEW"; }
case_g2() { https_case g2 "$NEW" "$OLD"; }

# --- The run -----------------------------------------------------------------------

echo "old: $OLD_SAYS ($OLD); new: $NEW_SAYS ($NEW)"
FAILED=0
for c in ${COMPAT_CASES:-a1 a2 a3 a4 b c d1 d2 e f g1 g2}; do
    CASE=$c
    C=$WORK/$c
    mkdir -p "$C"
    : >"$C/pids"
    : >"$C/lines"
    SNAP=()
    (
        set -eEuo pipefail
        trap '[ -s "$C/lines" ] || echo "$CASE FAILED: line $LINENO: \`$BASH_COMMAND\` failed" >>"$C/lines"' ERR
        # shellcheck disable=SC2046 # one pid a word
        trap 'stop $(cat "$C/pids")' EXIT
        "case_$c"
    ) >"$C/case.out" 2>&1
    rc=$?
    [ -s "$C/lines" ] || echo "$c FAILED: exit $rc, nothing said" >>"$C/lines"
    cat "$C/lines"
    if grep -q ' FAILED: ' "$C/lines"; then
        FAILED=$((FAILED + 1))
        {
            echo "--- $c: what it printed"
            tail -20 "$C/case.out"
            for f in "$C"/*.log; do
                [ -e "$f" ] || continue
                echo "--- $f"
                tail -12 "$f"
            done
        } >&2
    fi
done
if [ "$FAILED" -gt 0 ]; then
    echo "compat-matrix: $FAILED case(s) differ"
    exit 1
fi
echo "compat-matrix: every case held"
