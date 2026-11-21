#!/usr/bin/env bash
# Two releases' processes beside each other (docs/design/compatibility.md,
# "The compatibility matrix"): the last published release's binaries,
# OLD_BIN_DIR, as tools/release-bins.sh takes them from its assets, and this
# tree's, NEW_BIN_DIR, each case driven both ways where it has two:
#
#   tools/compat-matrix.sh OLD_BIN_DIR NEW_BIN_DIR
#   tools/compat-matrix.sh --self-test
#
# Each directory holds `fennec-server`, `fennec` and `fennec-shard`
# (`make compat-check` builds this tree's and takes the release's; the old
# router is built from the release's tag, which publishes none). The policy
# it holds is RELEASING.md's (*Every reader upgrades before any primary*):
# a binary one release back beside a newer one works or stops in words, and
# never reads wrong -- naming the record kind, tag or version where it names
# one -- and a newer reader reads what an older writer wrote, but for the
# values and types no write makes any more that compatibility.md lists. The
# cases, each printing one line a part, CASES below their list:
#
#   a1  an old primary and a new replica: the first HALF writes in the
#       replica's one image (`images` 1), the rest as records, its reads and
#       its file opened again the primary's;
#   a2  a new primary and an old replica, placed by position: the primary's
#       change counter after each write against the replica's, so a stop is
#       put on the write it is at, never on a later one; there the write
#       must be one of UNKNOWN_TO_OLD (by its text) and the stop's words its
#       entry's, the kind or tag named where the old binary names one; its
#       reads and its file, opened by the old binary, the primary's then;
#   a3  an old primary's `[[json]]` row nested 66 levels deep, which this
#       tree refuses where it reads it (crates.md, "How deep a value
#       nests"): a new replica takes an image and holds it, a new server
#       opens the file, and each answers what compatibility.md says;
#   a3b an old file whose schema holds a type of 65 nested lists, refused by
#       this tree's server at the open, in words;
#   a4  what the old release writes and this tree no longer takes -- a
#       stored definition holding 1e400, a vector component of 1e39 --
#       followed by a new replica and read there as the old primary reads it;
#   b1-b4  archives: the old `fennec archive`'s, plain and sealed (version 1),
#       verified and restored by the new `fennec`, to the end and to a moment;
#       the new archiver's, plain, read by the old `fennec`, and sealed
#       (version 3), refused by it in RELEASING.md's words;
#   c1-c3  the old `fennec backup`, plain and sealed, opened by the new
#       server; a file the old server wrote, opened and written by the new,
#       then verified, typed and opened by the old; the new backup opened, or
#       refused in words, by the old;
#   d1-d2  an old router over new nodes and a new router over old nodes: a
#       tenant created, written, copied to its replica, moved and deleted;
#       the new nodes under the old router, asking no token, run
#       `--insecure` (`insecure_under`), as g1's;
#   e   a rolling upgrade under durable writes: a router and two `--dir`
#       nodes, one the other's standby, all of the release, upgraded in
#       RELEASING.md's and deploy/RUNBOOK.md's order -- the router, the
#       standby's node, then the primary's, the runbook's condition met
#       between the nodes -- each write classed acknowledged, not sent or
#       unknown and read back from both nodes at the end. Every restart is a
#       graceful TERM: what a kill keeps is the power-cut and chaos rigs';
#   f1-f2  a `github` hook kept with `key delivery`, which RELEASING.md says
#       the release refuses: a single server's hooks file line, and a
#       tenant's set kept by an old node, the router and node upgraded;
#   g1-g2  a node serving HTTPS, recorded at https:// by a router of the
#       other release.
#
# Not driven, each for its reason (compatibility.md says the same):
# `token_expired` and a seed sent in parts (#665) are the sync client's,
# the native library's and the browser module's, neither in the release's
# tarball nor reachable from a shell -- an old client against a new server
# is T4's; a copy keeping its primary's write times is driven by no case
# (b's archivers follow a primary, not a replica of the other release); a
# replica's follow record synced before the follow is answered is e's only
# as far as a graceful restart reads the copy's role the old node wrote,
# its durability under a kill being the power-cut rig's; OP_MARK, REC_SPILL
# and REC_LAND as FORMAT says.
#
# FORMAT holds every record kind, type tag and op of the tree (every
# REC_*, TAG_* and OP_* `u8` constant of crates/fennec-core/src, whatever
# its file and visibility) to a write of HISTORY that makes it, or to a
# reason none is needed; a run and the self-test fail on one it does not
# name, so the next addition cannot be forgotten. The other numbered spaces
# a file or a stream carries are tables of their own, held the same way and
# in both directions (a code with no line, a line with no code): GRAPHS
# (the graph record's versions), INDEX_KINDS (the byte behind a field's
# index), SMALL_CODES (metric, quantization and collation codes) and STREAM
# (the replication stream's message tags). Where a space is literals and not
# constants, the check reads the match arms of the function that owns it
# (`arm_codes`) and the table is required to be the same list.
#
# COMPAT_CASES, where set, names the cases to run (`a2 e`): a case that
# failed under load is run again alone before it is believed.
#
# Held to two bashes: macOS's /bin/bash 3.2, which runs `make compat-check`
# on a Mac, and the GitHub Linux runner's (5.x); `--self-test` is run under
# both. 3.2 differs where it counts here: a subshell with an EXIT trap that
# dies on `set -u` exits 0 (so a case's status alone cannot be believed: each
# case writes a `finished` sentinel after it returns, required beside its
# parts), `set -e` does not act inside a function used as an `if` condition
# (so a check in a helper is `|| fail`, an `exit`, never a bare command: the
# toy case `t_cond` holds it), no associative arrays and no namerefs.
#
# A case runs in a subshell of its own under `set -eE`: a command failing
# anywhere in it fails it, its line and command written down whatever it
# printed before, a case that exits other than 0 fails, and each part a
# case owes (CASES) must print exactly one line of its own: a part unsaid
# fails by name, and the run ends held to the whole list. The script exits 1
# where any part failed or went unsaid. A difference is one of three:
# promised in RELEASING.md or compatibility.md, which the case asserts in
# the promised words; a break nobody wrote down; or a bug. The last two fail
# the case until someone decides: nothing here passes over one.
#
# Every server listens on port 0, found by its `listening on:` line, and a
# server restarted takes port 0 again and is told to whatever finds it --
# but for e's primary node, whose standby follows it by the address it was
# started with: a machine keeps its address across `systemctl restart`, so
# that node comes back at its own. Everything started is killed and every
# path made removed on the way out, and a failure prints the logs' tails.
# COMPAT_LOGS, where set, is a directory the logs are copied into before
# they go. Needs curl, jq and openssl; --self-test, jq alone.

set -uo pipefail
# One collation for sort and comm, which reads its lines in sort's order.
export LC_ALL=C

usage='usage: tools/compat-matrix.sh OLD_BIN_DIR NEW_BIN_DIR | --self-test'
ROOT=$(cd "$(dirname "$0")/.." && pwd)

# --- What a case does with ---------------------------------------------------

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

# The line a part of a case ends on.
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

# up LOG PID: waits, bounded, for the server PID to say where it listens (the
# address printed, status 0) or to exit (1: its log says why); 2 where it did
# neither in 30 s. awk reads the log whole, as no pipe closed early would.
up() {
    local log=$1 pid=$2 line
    for _ in $(seq 1 600); do
        line=$(awk '/listening on: http:\/\//{ match($0, /http:\/\/[^ ]*/); print substr($0, RSTART, RLENGTH); exit }' "$log" 2>/dev/null || true)
        if [ -n "$line" ]; then
            echo "$line"
            return 0
        fi
        kill -0 "$pid" 2>/dev/null || return 1
        sleep 0.05
    done
    return 2
}

# The address a server said it listens on, waited for; the case's failure
# where it exits or says none.
addr() {
    local log=$1 pid=$2 a rc=0
    a=$(up "$log" "$pid") || rc=$?
    case $rc in
    0) echo "$a" ;;
    1) fail "$(basename "$log" .log) exited before it listened: $(tail -3 "$log" | tr '\n' ' ')" ;;
    *) fail "$(basename "$log" .log) said no address in 30 s" ;;
    esac
}

# refused_start LOG CMD...: CMD, a server that must refuse to start, started
# in the background as every other is and waited for by one of two events,
# bounded: its exit (REFUSED_RC its status) or its `listening on:` line, which
# fails the case by its name. Run in the foreground, a refusal that regressed
# started the server and the run hung to the job's timeout, naming no case.
refused_start() {
    local log=$1 pid
    spawn "$@"
    pid=$SPAWNED
    for _ in $(seq 1 600); do
        if grep -q 'listening on: ' "$log" 2>/dev/null; then
            stop "$pid"
            fail "the server started where it must refuse, so the refusal $CASE holds has regressed: $(tail -2 "$log" | tr '\n' ' ' | bare)"
        fi
        if ! kill -0 "$pid" 2>/dev/null; then
            REFUSED_RC=0
            wait "$pid" || REFUSED_RC=$?
            return 0
        fi
        sleep 0.05
    done
    stop "$pid"
    fail "the server neither exited nor listened in 30 s: $(tail -2 "$log" | tr '\n' ' ' | bare)"
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

# w URL QUERY [TOKEN]: a write the case needs, answered 2xx and no error,
# anything else the case's end.
w() {
    local code
    code=$(curl -sS --max-time 60 -o "$C/w.out" -w '%{http_code}' -H "Authorization: Bearer ${3:-t}" \
        -H 'Content-Type: application/json' -d "$(jq -cn --arg q "$2" '{query: $q}')" "$1/query") ||
        fail "\`$2\` had no answer from $1"
    case $code in
    2??) ;;
    *) fail "\`$2\` was answered $code by $1: $(head -c 300 "$C/w.out")" ;;
    esac
    if grep -q '"error"' "$C/w.out"; then
        fail "\`$2\` was answered $code with an error by $1: $(head -c 300 "$C/w.out")"
    fi
}

# admin URL METHOD PATH [BODY] [TOKEN]: the status; the body in $C/admin.out.
admin() {
    curl -sS --max-time 60 -o "$C/admin.out" -w '%{http_code}' -X "$2" \
        -H "Authorization: Bearer ${5:-root}" ${4:+-d "$4"} "$1$3"
}

# status URL [TOKEN]: URL's /_replication/status.
status() {
    curl -sS --max-time 30 -H "Authorization: Bearer ${2:-r}" "$1/_replication/status"
}

# seq_of URL: the change counter URL's status says it is at.
seq_of() {
    status "$1" | jq -er .seq
}

# The history every case writes. Each record kind, type tag and op a write
# makes comes from one of these (FORMAT names which); kind 13's ops 1 to 4
# and 6, tags 11 to 18 among them. A release that adds a kind, a tag or an
# op adds a write making it here and its line to FORMAT; where the last
# release does not read it, its text to UNKNOWN_TO_OLD.
HISTORY=(
    "create collection notes (title text nocase @hash @text, kind text @hash, n int @sorted, meta json)"
    "create collection docs (v vector<3> @hnsw(cosine), label text @text)"
    "create collection ledger (o int)"
    "create collection kinds (b bool, f float, raw bytes, h vector<2, f16>, at timestamp, sp sparse<8>, name text collate tr, place geo @geo, gone int, k1 int, k2 text, @sorted(k1, k2))"
    "put notes [{title: 'Hello', kind: 'open', n: 1, meta: {a: [1, 2], b: 'x', c: true, f: 1.5}}, {title: 'two', kind: 'open', n: 2, meta: null}, {title: 'three', kind: 'void', n: 3}]"
    "put docs [{v: [1, 0, 0.5], label: 'first'}, {v: [0, 1, 0], label: 'second'}, {v: [0.25, 0.25, 1], label: 'third'}]"
    "create collection indexed (code text @unique, born timestamp @ttl(30d), cn text @text(chars), ac text @text(accents), v8 vector<3> @hnsw(cosine, quant=int8), vb vector<8> @hnsw(cosine, quant=bit), vl vector<3> @hnsw(l2), vd vector<3> @hnsw(dot), lang text collate und, s sparse<8> @inverted)"
    "put kinds [{b: true, f: 1.5, raw: 'aGVsbG8=', h: [0.5, 0.25], at: '2026-11-15T10:00:00Z', sp: '{1:0.5,7:0.25}/8', name: 'Ilık', place: [13.4, 52.5], gone: 1, k1: 1, k2: 'a'}, {b: false, name: 'ırmak', k1: 2, k2: 'b'}]"
    "put indexed [{code: 'a1', born: '2099-01-01T00:00:00Z', cn: 'Ilık', ac: 'Café', v8: [1, 0, 0.5], vb: [1, 0, 0, 0, 0, 0, 0, 0], vl: [0, 1, 0], vd: [0.25, 0.25, 1], lang: 'ä', s: '{1:0.5,7:0.25}/8'}, {code: 'b2', born: '2099-01-02T00:00:00Z', cn: 'ırmak', ac: 'cafe', v8: [0, 1, 0.5], vb: [0, 1, 0, 0, 0, 0, 0, 0], vl: [1, 0, 0], vd: [1, 1, 0.25], lang: 'z', s: '{2:1}/8'}]"
    "create statement titles() as get notes select title order n"
    "create statement gone() as get notes count"
    "create statement paid after set notes where new.kind = 'paid' as insert ledger {o: new.id}"
    "create statement tidy every 1d as del notes where kind = 'gone'"
    "create statement two() public as begin insert ledger {o: 100}; insert ledger {o: 101} end"
    "create statement pushed(\$sha text from event.after) on hook gh where hook.event = 'push' as insert ledger {o: 7}"
    "drop statement gone"
    "call two"
    "set notes {kind: 'paid'} where title = 'HELLO'"
    "del notes where n = 3"
    "alter collection notes add field extra text"
    "create index on ledger (o) @sorted"
    "alter collection kinds drop field gone"
    "create collection scratch (x int)"
    "drop collection scratch"
    "put notes {title: 'after', kind: 'open', n: 4, extra: 'x'}"
    "set docs {label: 'moved'} where label = 'second'"
    "put notes {title: 'last', kind: 'open', n: 5}"
)
# Where a replica started after the history began takes its image.
HALF=12

# Where the numbered spaces live, relative to the tree's root.
CORE_SRC=crates/fennec-core/src
# NAME|the start of the write of HISTORY that makes it, or NAME|- why none
# is needed. Every REC_*, TAG_* and OP_* constant of CORE_SRC has its line,
# and no line names one the tree no longer has (check_tables).
FORMAT=(
    "REC_CREATE|create collection notes"
    "REC_DROP|drop collection scratch"
    "REC_DATA|put notes ["
    "REC_GRAPH|- not a write: an image holds it (a1's and a2's replicas each take one, docs' graph in it), and a server appends it as it saves its graphs"
    "REC_ALTER|create index on ledger (o) @sorted"
    "REC_SEQ|- not a write: the head of every image (a1's and a2's replicas each take one)"
    "REC_NEXTID|- not a write: an image holds one a collection (a1's and a2's replicas each take one)"
    "REC_HISTORY|- not a write and never sent as one: a replica takes its primary's history from the stream, and a file's own is written as it is made and promoted (a1 and a2 open a copy with --promote)"
    "REC_BLOCK|set notes {kind: 'paid'}"
    "REC_SPILL|- a block past 16 MB, in its primary's file alone: never sent (the feed sends the block as REC_BLOCK), and a write of 16 MB in every case's history costs the run more than it would show"
    "REC_LAND|- a spilled block's landing, in its primary's file alone: REC_SPILL's reason"
    "REC_FIELDS|alter collection notes add field extra text"
    "REC_STATEMENT|create statement titles()"
    "REC_SYNCED|- not a write and never sent: a file's server appends it as it closes, which c2's file, closed by the new server and opened by the old, holds"
    "OP_PUT|put notes ["
    "OP_DEL|del notes where n = 3"
    "OP_DEFINE|create statement titles()"
    "OP_DROP|drop statement gone"
    "OP_BOUND|create statement paid after"
    "OP_SCHEDULED|create statement tidy every 1d"
    "OP_MARK|- a scheduled run's mark, written by the primary's clock at a slot: a slot each second would move the change counter between the history's writes, by which a2 places the old replica"
    "OP_HOOKED|create statement pushed("
    "TAG_NULL|put notes ["
    "TAG_BOOL|put kinds ["
    "TAG_INT|put notes ["
    "TAG_FLOAT|put kinds ["
    "TAG_TEXT|put notes ["
    "TAG_BYTES|put kinds ["
    "TAG_VECTOR|put docs ["
    "TAG_LIST|put notes ["
    "TAG_VECTOR_F16|put kinds ["
    "TAG_TIMESTAMP|put kinds ["
    "TAG_SPARSE|put kinds ["
    "TAG_COLLATED|create collection kinds"
    "TAG_DROPPED|alter collection kinds drop field gone"
    "TAG_OBJECT|put notes ["
    "TAG_JSON|create collection notes"
    "TAG_GEO|create collection kinds"
    "TAG_COMPOSITES|create collection kinds"
    "TAG_NOCASE|create collection notes"
    "TAG_BESIDE|create collection notes"
)
# A write of HISTORY the last release does not read, by its text, and the
# words (an extended regex) its replica must stop with: the kind or tag
# number where the old binary names one -- `record kind 15`, `type tag 19`
# -- or `a statement record this version does not read` for an op. Empty:
# every kind, tag and op this tree writes is 0.2.0's (kinds 1 to 14, tags 0
# to 18, ops 1 to 6).
UNKNOWN_TO_OLD=()
# The graph record's version byte (vector.rs's GRAPH_VERSION*), in the same
# form. A server writes 9 into an image and 10 into its file's tail as it
# saves its graphs (`save_graphs`, under the read lock, when a graph is due:
# a server's own timing, not a write's, so REC_GRAPH's reason); 3 to 8 are
# written by no build any more. The reader of a record it does not restore
# rebuilds the graph from the documents (`restore_graph` answers None): the
# graph is derived data, never read wrong.
GRAPHS=(
    "GRAPH_VERSION|- read only: no build writes version 3 any more, and a record of it restores or is rebuilt from the documents (vector.rs's test a_flat_graph_record_restores_as_the_varint_one_did writes the older layouts with the test-only serialize_varint and restores them)"
    "GRAPH_VERSION_QUANT|- read only, as version 3's reason"
    "GRAPH_VERSION_UNLINKED|- read only, as version 3's reason"
    "GRAPH_VERSION_KEPT|- read only, as version 3's reason"
    "GRAPH_VERSION_FLAT|- read only, as version 3's reason"
    "GRAPH_VERSION_FLAT_KEPT|- read only, as version 3's reason"
    "GRAPH_VERSION_ALIASED|put docs ["
    "GRAPH_VERSION_ALIASED_KEPT|- written by a server's own timing into its file's tail (REC_GRAPH's reason), never by a write: a1's and a2's replicas take version 9 in an image, and a file's tail is read by whichever release opens it next"
)
# The byte `encode_index` writes behind a field's index (schema.rs), a
# literal and no constant: the check reads `encode_index`'s `out.push(N)`s
# and `decode_index`'s arms, requires the two to be one list, and the table
# to be it. KIND|write or reason, as FORMAT's.
INDEX_KINDS=(
    "0|create collection ledger"
    "1|create collection notes"
    "2|create collection docs"
    "3|create collection notes"
    "4|create collection notes"
    "5|create collection indexed"
    "6|create collection indexed"
    "7|create collection indexed"
    "8|create collection indexed"
    "9|create collection indexed"
    "10|create collection kinds"
    "11|create collection indexed"
)
# The codes a schema writes for a vector index's metric (`Metric::code`), its
# quantization (`Quant::code`) and a field's collation (`Collation::code`),
# literals in match arms the check reads (`from_code`'s: the reader is what
# refuses an unknown one). KIND:CODE|write or reason.
SMALL_CODES=(
    "metric:0|create collection docs"
    "metric:1|create collection indexed"
    "metric:2|create collection indexed"
    "quant:0|- never written: an index with no quantization is kind 2 and carries no quantization byte (encode_index); a 0 behind kind 5 is read as none"
    "quant:1|create collection indexed"
    "quant:2|create collection indexed"
    "collation:1|create collection kinds"
    "collation:2|create collection indexed"
)
# The replication stream's message tags (`[tag: u8][length][payload]`,
# crates/fennec-http/src/replication.rs): `b'X'` literals the primary
# writes (`message(.., b'X', ..)`) and the replica reads (`b'X' =>`), the
# two to be one list and the table to be it. The stream is the primary's
# answer to a replica, the one place a tag crosses releases.
STREAM=(
    "H|- the hello opens every stream: a1's and a2's, each way"
    "I|- a1's and a2's replicas take their image in it, each way"
    "W|- the writes after the image, each way: a1's and a2's histories"
    "K|- a primary with nothing to send says it is there (ALIVE, 2 s): a3's replica waits steady at its primary's counter, the old primary to the new replica; the other way no case waits two seconds idle"
    "E|- the stream's end, text only: e's primary node closing sends it to its standby, the old to the new; the new primary's to the old replica is not driven (a2's primary is stopped after its replica)"
)
READS=(
    "get notes order id"
    "get docs select id, v, label order id"
    "get ledger order id"
    "get kinds order id"
    "call titles"
    "describe notes"
    "describe kinds"
    "statements"
    "get notes where title = 'HELLO' select id, title"
    "get notes match title 'hello' select id"
    "get docs near v [1, 0, 0.4] limit 2 select id, label"
    "get kinds order name collate tr select id, name"
    "get kinds where k1 = 2 and k2 = 'b' select id"
    "get indexed order id"
    "get indexed where code = 'b2' select id"
)

# Every read of READS at URL, one normalized answer a line: a refusal is
# `"error"`, whatever its words, so states of two versions compare; a
# score is left out (the order is the ranking) and so are a schedule's
# `next` and `last`, which the clock moves.
state() {
    local r
    for r in "${READS[@]}"; do
        q "$1" "$r" "${2:-t}" |
            jq -S -c 'if type == "object" and has("error") then "error" else . end
                | walk(if type == "object" then del(._score, .next, .last) else . end)' 2>/dev/null ||
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
    { diff <(echo "$1") <(echo "$2") || true; } |
        awk '/^[<>]/ && n < 2 { n++; print substr($0, 1, 300) }' | tr '\n' ' '
}

# answered WHAT STATE: every read of STATE was answered and none refused:
# two refusals would compare equal whatever each said.
answered() {
    local bad
    bad=$(printf '%s\n' "$2" | awk -v ORS=' ' '/^"error"$/ || /^no answer to / { n++; if (n <= 2) print NR ": " substr($0, 1, 80) }')
    [ -z "$bad" ] || fail "$1: a read was refused or not answered ($bad)"
}

# state_holds OLD NEW: every answer of OLD, an older release's, is held by
# the same answer of NEW: each key it has is there with its value (an array
# as long as it was), so a field a later release adds to `describe` or
# `statements` fails nothing, and a key that changed or is gone does. A
# state with a line that is no JSON (`no answer to ...`) holds nothing.
state_holds() {
    jq -n -e --slurpfile o <(printf '%s\n' "$1") --slurpfile n <(printf '%s\n' "$2") '
        def holds($o; $n):
            if ($o | type) == "object" then
                ($n | type) == "object" and
                    ([$o | keys_unsorted[] as $k | ($n | has($k)) and holds($o[$k]; $n[$k])] | all)
            elif ($o | type) == "array" then
                ($n | type) == "array" and ($o | length) == ($n | length) and
                    ([range(0; $o | length) | holds($o[.]; $n[.])] | all)
            else $o == $n end;
        ($o | length) == ($n | length) and ([range(0; $o | length) | holds($o[.]; $n[.])] | all)' >/dev/null 2>&1
}

# until_holds URL OLD [SECONDS] [TOKEN]: waits for URL's state to hold OLD's.
until_holds() {
    local tries=$((${3:-60} * 10))
    for _ in $(seq 1 "$tries"); do
        state_holds "$2" "$(state "$1" "${4:-t}")" && return 0
        sleep 0.1
    done
    return 1
}

# first_line CMD...: the first line CMD says, whatever its status.
first_line() {
    local o
    o=$("$@" 2>&1) || true
    printf '%s\n' "${o%%$'\n'*}"
}

# first_image DIR: the first image file of an archive, none where it has none.
first_image() {
    find "$1" -name 'image-*' | awk 'NR == 1'
}

# verified_to ARCHIVE SEQ [KEY_FILE]: waits, bounded, for the new `fennec
# verify` of ARCHIVE to say it reaches change SEQ. The output is kept whole
# before it is looked at: under pipefail, `verify | grep -q` took the SIGPIPE
# of a grep that had found its line for a failure, and its `break` was missed.
verified_to() {
    local out=
    for _ in $(seq 1 600); do
        out=$("$NEW/fennec" verify "$1" ${3:+--key-file "$3"} 2>&1 || true)
        case $out in
        *"to $2,"*) return 0 ;;
        esac
        sleep 0.05
    done
    fail "the archive $(basename "$1") did not reach change $2 in 30 s: $(echo "$out" | tail -2 | tr '\n' ' ' | bare)"
}

# history URL FROM TO [STATUS_URL]: HISTORY's writes FROM to TO
# (exclusive), URL's state after each kept in SNAP[i+1] and, with
# STATUS_URL, its change counter in PSEQ[i+1].
history() {
    local i
    for ((i = $2; i < $3; i++)); do
        w "$1" "${HISTORY[$i]}"
        SNAP[i + 1]=$(state "$1")
        if [ -n "${4:-}" ]; then
            PSEQ[i + 1]=$(seq_of "$4") || fail "no change counter from $4 after write $((i + 1))"
        fi
    done
}
SNAP=()
PSEQ=()

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
    local P=$ADDR n=${#HISTORY[@]}
    history "$P" 0 "$HALF"
    # Restarted, its ring of writes is empty, so a replica starting from
    # nothing takes an image: the first HALF writes come in it, the rest as
    # records (a primary whose ring held them all would send records alone).
    stop "$PID"
    server primary-again "$OLD" --file "$C/p.fennec" --http-token t --replication-token r
    P=$ADDR
    server replica "$NEW" --file "$C/r.fennec" --http-token t --replication-token r --replica-of "$P"
    local R=$ADDR RPID=$PID
    history "$P" "$HALF" "$n"
    local want=${SNAP[$n]} images
    answered "the old primary's reads of its own history" "$want"
    if ! until_holds "$R" "$want"; then
        fail "the new replica's reads do not hold the old primary's: $(first_difference "$want" "$(state "$R")"); its log: $(grep -m1 -i 'could not\|refus' "$C/replica.log")"
    fi
    images=$(status "$R" | jq -r .images)
    [ "$images" = 1 ] ||
        fail "the new replica says it took $images images, not the one holding the first $HALF writes: $(status "$R")"
    stop "$RPID"
    opened reopened "$NEW" "$(copy_of "$C/r.fennec")" --promote
    state_holds "$want" "$OPENED" ||
        fail "the new replica's file, opened again, does not hold what the old primary answered: $(first_difference "$want" "$OPENED")"
    held a1 "the new replica followed the old primary's $n writes, the first $HALF in its one image (\`images\` 1) and the rest as records; its reads and its file opened again are the primary's"
}

# The text of UNKNOWN_TO_OLD's entry for a write, and its stop words.
unknown_entry() {
    local e
    for e in ${UNKNOWN_TO_OLD[@]+"${UNKNOWN_TO_OLD[@]}"}; do
        if [ "${e%%|*}" = "$1" ]; then
            echo "${e#*|}"
            return 0
        fi
    done
    return 1
}

case_a2() {
    server primary "$NEW" --file "$C/p.fennec" --http-token t --replication-token r
    local P=$ADDR n=${#HISTORY[@]} i
    SNAP[0]=$(state "$P")
    PSEQ[0]=$(seq_of "$P")
    history "$P" 0 "$HALF" "$P"
    stop "$PID"
    server primary-again "$NEW" --file "$C/p.fennec" --http-token t --replication-token r
    P=$ADDR
    server replica "$OLD" --file "$C/r.fennec" --http-token t --replication-token r --replica-of "$P"
    local R=$ADDR RPID=$PID
    history "$P" "$HALF" "$n" "$P"
    answered "the new primary's reads of its own history" "${SNAP[$n]}"
    # Each write moved the counter, so a position names one write.
    for ((i = 1; i <= n; i++)); do
        [ "${PSEQ[$i]}" -gt "${PSEQ[$((i - 1))]}" ] ||
            fail "write $i, \`${HISTORY[$((i - 1))]}\`, left the primary's counter at ${PSEQ[$i]}: a position would name two writes"
    done
    # The replica at the primary's last position, or stopped short of it
    # and standing there (a second of the same counter after its stop).
    local at= last= steady=0 k= stopped= words
    for _ in $(seq 1 600); do
        at=$(seq_of "$R" 2>/dev/null || true)
        [ "$at" = "${PSEQ[$n]}" ] && break
        if grep -q 'could not apply' "$C/replica.log" && [ -n "$at" ] && [ "$at" = "$last" ]; then
            steady=$((steady + 1))
            if [ "$steady" -ge 20 ]; then break; fi
        else
            steady=0
        fi
        last=$at
        sleep 0.05
    done
    for ((i = 0; i <= n; i++)); do
        if [ "${PSEQ[$i]}" = "$at" ]; then
            k=$i
            break
        fi
    done
    [ -n "$k" ] ||
        fail "the old replica stands at change '$at', where no write of the primary's history ended (${PSEQ[*]}): $(grep -m1 'could not apply' "$C/replica.log" || true)"
    local got
    got=$(state "$R")
    # What the old release answers is held by the new primary's answer: a
    # key a later release adds to `describe` or `statements` is no difference.
    state_holds "$got" "${SNAP[$k]}" ||
        fail "the old replica stands where write $k left the primary, but its reads are not held by the primary's then: $(first_difference "${SNAP[$k]}" "$got")"
    if [ "$k" -lt "$n" ]; then
        stopped=$(grep -m1 'could not apply' "$C/replica.log" || true)
        words=$(unknown_entry "${HISTORY[$k]}") ||
            fail "the old replica stopped at write $((k + 1)), \`${HISTORY[$k]}\`, which UNKNOWN_TO_OLD does not name ($OLD_SAYS reads it): ${stopped:-it said nothing}"
        grep -Eq "$words" <<<"$stopped" ||
            fail "the old replica stopped at write $((k + 1)), \`${HISTORY[$k]}\`, without the words UNKNOWN_TO_OLD gives it ($words): ${stopped:-it said nothing}"
    else
        local images
        images=$(status "$R" | jq -r .images)
        [ "$images" = 1 ] ||
            fail "the old replica says it took $images images, not the one holding the first $HALF writes: $(status "$R")"
    fi
    stop "$RPID"
    opened reopened "$OLD" "$(copy_of "$C/r.fennec")" --promote
    [ "$OPENED" = "$got" ] ||
        fail "the old replica's file, opened by $OLD_SAYS, is not what it answered: $(first_difference "$got" "$OPENED")"
    if [ "$k" -lt "$n" ]; then
        held a2 "the old replica stopped in UNKNOWN_TO_OLD's words at write $((k + 1)), \`${HISTORY[$k]}\` (${stopped}), its counter the primary's after write $k; its reads and its file, opened by the old binary, the primary's then"
    else
        held a2 "the old replica followed all ${n} writes of the new primary, the first $HALF in its one image, to the primary's counter (${PSEQ[$n]}); every kind, tag and op FORMAT names is one $OLD_SAYS reads; its reads -- describe, statements, nocase, match, near among them -- and its file, opened by the old binary, the primary's"
    fi
}

# The answer this tree gives a read of a row nested past 64 levels, and a
# write that would decode it: compatibility.md's exception (i).
DEEP_REFUSED='{"error":"a value nests more than 64 levels deep"}'

# compacted_ok URL: a `compact` of URL answered "compaction done". The answer
# is kept whole before it is looked at (a pipe into `grep -q` ends early, and
# under pipefail the writer's SIGPIPE read as a failure).
compacted_ok() {
    local out
    out=$(q "$1" compact) || return 1
    case $out in
    *'"message":"compaction done'*) return 0 ;;
    esac
    echo "$out" | cut -c1-200 >>"$C/compact.refused.log"
    return 1
}

# answers URL QUERY WANT WHAT: QUERY's answer at URL is WANT exactly, or
# the case ends naming WHAT.
answers() {
    local got
    got=$(q "$1" "$2")
    [ "$got" = "$3" ] || fail "$4: \`$2\` answered $(echo "$got" | cut -c1-200), not $3"
}

case_a3() {
    local deep='1' i
    for ((i = 0; i < 63; i++)); do deep="[$deep]"; done
    server primary "$OLD" --file "$C/p.fennec" --http-token t --replication-token r
    local P=$ADDR
    w "$P" "create collection other (n int, s text)"
    w "$P" "put other [{n: 1, s: 'a'}, {n: 2, s: 'b'}]"
    # A field before the deep one, and one after it.
    w "$P" "create collection deep (name text, v [[json]], after int)"
    w "$P" "put deep {name: 'deep', v: [[$deep]], after: 7}"
    # And one whose deep field is the last it stores: dropping a deep field
    # reads the row only where no field is stored after it (below).
    w "$P" "create collection deeplast (name text, v [[json]])"
    w "$P" "put deeplast {name: 'last', v: [[$deep]]}"
    # The old primary reads both whole.
    q "$P" "get deep order id" | jq -e 'length == 1 and .[0].after == 7' >/dev/null ||
        fail "$OLD_SAYS does not read the row it took: $(q "$P" "get deep order id" | cut -c1-200)"
    q "$P" "get deeplast order id" | jq -e 'length == 1 and .[0].name == "last"' >/dev/null ||
        fail "$OLD_SAYS does not read the row it took, its deep field last: $(q "$P" "get deeplast order id" | cut -c1-200)"
    # Restarted, so the new replica starts from an image holding the row.
    stop "$PID"
    server primary-again "$OLD" --file "$C/p.fennec" --http-token t --replication-token r
    P=$ADDR
    local PP=$PID
    server replica "$NEW" --file "$C/r.fennec" --http-token t --replication-token r --replica-of "$P"
    local R=$ADDR RP=$PID
    w "$P" "put deep {name: 'shallow', v: [[1]], after: 8}"
    local pseq other st=
    pseq=$(seq_of "$P")
    other=$(q "$P" "get other order id")
    # Its steady state: connected, an image taken, at the primary's
    # counter, the counts the primary's (not the first "could not apply").
    for _ in $(seq 1 600); do
        st=$(status "$R" 2>/dev/null || true)
        if echo "$st" | jq -e --argjson s "$pseq" '.connected == true and .images >= 1 and .seq == $s and .behind == 0' >/dev/null 2>&1 &&
            [ "$(q "$R" "get deep count")" = '[{"count":2}]' ] &&
            [ "$(q "$R" "get other count")" = '[{"count":2}]' ]; then
            break
        fi
        st=
        sleep 0.05
    done
    [ -n "$st" ] ||
        fail "the new replica did not reach its primary's counter $pseq with an image and the counts: $(status "$R" 2>&1 | cut -c1-300); its log: $(grep -m1 'could not apply' "$C/replica.log" || true)"
    answers "$R" "get deep order id" "$DEEP_REFUSED" "the new replica's read of the deep row"
    answers "$R" "get deeplast order id" "$DEEP_REFUSED" "the new replica's read of the row whose deep field is last"
    answers "$R" "get other order id" "$other" "the new replica's other collection"
    answers "$R" "get deep where id = 2" '[{"id":2,"name":"shallow","v":[[1]],"after":8}]' "the new replica's other row"
    stop "$RP" "$PP"

    # This tree's server opening the file 0.2.0 wrote.
    server reopen "$NEW" --file "$C/p.fennec" --http-token t --replication-token r
    local N=$ADDR NP=$PID
    answers "$N" "get other order id" "$other" "another collection of the file"
    answers "$N" "get deep count" '[{"count":2}]' "the deep collection's count"
    answers "$N" "get deep where id = 2" '[{"id":2,"name":"shallow","v":[[1]],"after":8}]' "the deep collection's other row"
    answers "$N" "get deep select name order id" '[{"name":"deep"},{"name":"shallow"}]' "a field stored before the deep one"
    answers "$N" "get deep order id" "$DEEP_REFUSED" "a read decoding the deep row"
    answers "$N" "get deeplast count" '[{"count":1}]' "the count of the collection whose deep field is last"
    answers "$N" "get deeplast select name" '[{"name":"last"}]' "the field stored before the deep one, the deep one last"
    answers "$N" "del deep where id = 1" "$DEEP_REFUSED" "a del of the deep row"
    answers "$N" "set deep {name: 'x'} where id = 1" "$DEEP_REFUSED" "a set of the deep row"
    answers "$N" "create index on deep (after) @sorted" "$DEEP_REFUSED" "an index over a field after the deep one"
    answers "$N" "get deep count" '[{"count":2}]' "the count after the refused writes"
    # A backup carries the row as it is.
    "$NEW/fennec" backup "$N" "$C/backup.fennec" --token r >"$C/backup.log" 2>&1 ||
        fail "the new \`fennec backup\` of the file failed: $(tr '\n' ' ' <"$C/backup.log")"
    # A compact without dropped fields copies the row's bytes, kept across
    # a reopen.
    compacted_ok "$N" || fail "a compact of the file (no dropped field) was refused"
    stop "$NP"
    server compacted "$NEW" --file "$C/p.fennec" --http-token t
    N=$ADDR
    answers "$N" "get deep count" '[{"count":2}]' "the deep collection's count after a compact and a reopen"
    answers "$N" "get deep order id" "$DEEP_REFUSED" "the deep row after a compact and a reopen"
    answers "$N" "get other order id" "$other" "another collection after a compact and a reopen"
    # Dropping the deep field. Every skip of a dropped place goes through
    # `codec::skip_field`, which refuses a list, an object or a point at
    # depth 64 as the decoder does, so the fields stored before the dropped
    # one read and any field after it is refused in the same words: the row
    # reads, and can be deleted, only where the deep field is the last of
    # its collection. Both layouts are in the file: `deeplast` (the deep
    # field last) and `deep` (a field after it).
    answers "$N" "del deeplast where id = 1" "$DEEP_REFUSED" "a del of the row whose deep field is last, before the drop"
    w "$N" "alter collection deeplast drop field v"
    answers "$N" "get deeplast order id" '[{"id":1,"name":"last"}]' "the row whose deep field, the last stored, was dropped"
    w "$N" "alter collection deep drop field v"
    answers "$N" "get deep select name order id" '[{"name":"deep"},{"name":"shallow"}]' "a field stored before the dropped deep one"
    answers "$N" "get deep select after order id" "$DEEP_REFUSED" "a field stored after the dropped deep one"
    answers "$N" "get deep order id" "$DEEP_REFUSED" "a read of every field, one stored after the dropped deep one"
    answers "$N" "del deep where id = 1" "$DEEP_REFUSED" "a del of the row with a field after its dropped deep one"
    # A compact asked for here is answered as this release answers it, which
    # changes between releases (refused, or taken): what holds either way is
    # that every other answer of the file is what it was, so each is read
    # again, on a reopened server too.
    q "$N" compact >/dev/null || true
    stop "$PID"
    server dropped "$NEW" --file "$C/p.fennec" --http-token t
    N=$ADDR
    answers "$N" "get other order id" "$other" "another collection after a compact was asked for"
    answers "$N" "get deeplast order id" '[{"id":1,"name":"last"}]' "the readable row after a compact was asked for"
    answers "$N" "get deep select name order id" '[{"name":"deep"},{"name":"shallow"}]' "the readable fields after a compact was asked for"
    answers "$N" "get deep select after order id" "$DEEP_REFUSED" "a field after the dropped deep one, after a compact was asked for"
    # deeplast: the readable row is deleted; deep: its readable rows are out
    # (selected above), the collection dropped.
    w "$N" "del deeplast where id = 1"
    answers "$N" "get deeplast count" '[{"count":0}]' "the collection whose row was deleted"
    w "$N" "drop collection deep"
    w "$N" "drop collection deeplast"
    q "$N" collections | jq -e '[.[].name] == ["other"]' >/dev/null ||
        fail "\`drop collection deep\` left: $(q "$N" collections | cut -c1-200)"
    answers "$N" "get other order id" "$other" "another collection after the deep one was dropped"
    stop "$PID"
    server backup "$NEW" --file "$C/backup.fennec" --http-token t
    answers "$ADDR" "get deep count" '[{"count":2}]' "the backup's deep collection"
    answers "$ADDR" "get deep order id" "$DEEP_REFUSED" "the backup's deep row"
    stop "$PID"
    held a3 "a \`[[json]]\` row nested 66 levels deep, which $OLD_SAYS takes and reads: this tree's replica takes it in an image and stands steady at its primary's counter (connected, \`behind\` 0, the counts equal), and this tree's server opens the file; each reads every other collection and row, \`count\` 2 and a field stored before the deep one, answers a read of the row exactly $DEEP_REFUSED, refuses its \`del\`, its \`set\` and an index over a later field in the same words, the count still 2; a backup and a compact (no dropped field) carry the row unchanged across a reopen; with the deep field dropped, the fields stored before it read, a field stored after it is refused in the same words (its row cannot be deleted either), a row whose deep field is the last of its collection reads whole and is deleted, and every other answer is what it was after a compact was asked for"
}

case_a3b() {
    local t='int' i
    for ((i = 0; i < 65; i++)); do t="[$t]"; done
    server primary "$OLD" --file "$C/p.fennec" --http-token t
    w "$ADDR" "create collection deeptype (v $t)"
    stop "$PID"
    local rc said_
    refused_start "$C/new.log" "$NEW/fennec-server" --file "$C/p.fennec" --http 127.0.0.1:0 --http-token t
    rc=$REFUSED_RC
    said_=$(grep -m1 'could not open' "$C/new.log" | bare || true)
    [ "$rc" = 1 ] && [ "$said_" = "could not open p.fennec: corrupt: a value nests more than 64 levels deep" ] ||
        fail "the new server opening a file whose type nests 65 lists exited $rc, saying: $(tr '\n' ' ' <"$C/new.log" | bare | cut -c1-300)"
    held a3b "a file whose schema holds a type of 65 nested lists, which $OLD_SAYS takes: this tree's server refuses it at the open, exit 1, \"$said_\""
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
    # The old primary's own answers are no refusal: two refusals would
    # compare equal whatever each said.
    case $want in
    *'"error"'*) fail "$OLD_SAYS refused a read of what it wrote: $want" ;;
    esac
    if until_state "$R" "$want" 30; then
        # The listing's other fields may grow between releases; whether the
        # definition is marked broken may not.
        broken=$(q "$R" statements | jq -c '[.[] | select(.name == "below") | .broken]' 2>/dev/null || echo "no listing")
        if [ "$broken" != '[null]' ]; then
            differs a4 "the new replica marks the old primary's definition holding 1e400 broken: $broken"
            return 0
        fi
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

# The magic a sealed file starts with, its version byte as a digit; nothing
# for no file (an archive that took no image), which its case then says.
sealed_version() {
    [ -n "$1" ] && [ -f "$1" ] || return 0
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
    answered "the old primary's reads of its own history" "$want"
    answered "the old primary's reads at the moment between its halves" "$moment_state"
    seq=$(seq_of "$P")
    for a in old-plain new-plain; do
        verified_to "$C/$a" "$seq"
    done
    for a in old-sealed new-sealed; do
        local key=$C/old.key
        [ "$a" = new-sealed ] && key=$C/new.key
        verified_to "$C/$a" "$seq" "$key"
    done
    stop "$A1" "$A2" "$A3" "$A4"

    # b1: the old archiver's plain archive, read by the new `fennec`.
    local why=
    "$NEW/fennec" verify "$C/old-plain" >"$C/b1-verify.log" 2>&1 ||
        why="the new verify refused it: $(tr '\n' ' ' <"$C/b1-verify.log")"
    if [ -z "$why" ]; then
        archived "$C/old-plain" "$NEW" "$C/b1-end.fennec"
        state_holds "$want" "$OPENED" || why="restored to its end: $(first_difference "$want" "$OPENED")"
    fi
    if [ -z "$why" ]; then
        archived "$C/old-plain" "$NEW" "$C/b1-at.fennec" --to "$at"
        state_holds "$moment_state" "$OPENED" || why="restored to $at: $(first_difference "$moment_state" "$OPENED")"
    fi
    if [ -z "$why" ]; then
        held b1 "the old archiver's archive verified by the new \`fennec\`, restored to its end (change $seq) and to a moment between two writes"
    else
        differs b1 "the old archiver's archive, read by the new \`fennec\`: $why"
    fi

    # b2: the old archiver's sealed archive (version 1), read by the new.
    why=
    local v
    v=$(sealed_version "$(first_image "$C/old-sealed")")
    [ "$v" = 1 ] || why="its image is no version 1 sealed file (version '$v')"
    if [ -z "$why" ]; then
        "$NEW/fennec" verify "$C/old-sealed" --key-file "$C/old.key" >"$C/b2-verify.log" 2>&1 ||
            why="the new verify refused it: $(tr '\n' ' ' <"$C/b2-verify.log")"
    fi
    if [ -z "$why" ]; then
        archived "$C/old-sealed" "$NEW" "$C/b2-end.fennec" --key-file "$C/old.key"
        state_holds "$want" "$OPENED" || why="restored to its end: $(first_difference "$want" "$OPENED")"
    fi
    if [ -z "$why" ]; then
        archived "$C/old-sealed" "$NEW" "$C/b2-at.fennec" --key-file "$C/old.key" --to "$at"
        state_holds "$moment_state" "$OPENED" || why="restored to $at: $(first_difference "$moment_state" "$OPENED")"
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

    # b4: the new archiver's sealed archive is version 3, which the old
    # `fennec` refuses as RELEASING.md says ("not a sealed file", "not sealed,
    # where the archive is"), writing nothing; the new reads it. Words that
    # name no version: 0.2.0 knew of no other.
    why=
    v=$(sealed_version "$(first_image "$C/new-sealed")")
    [ "$v" = 3 ] || why="its image is no version 3 sealed file (version '$v')"
    local verify_said= restore_said=
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
        state_holds "$want" "$OPENED" || why="the new restore of it: $(first_difference "$want" "$OPENED")"
    fi
    if [ -z "$why" ]; then
        held b4 "the new archiver's sealed archive (version 3) refused by the old \`fennec\` as promised, in words naming no version -- verify: \"$verify_said\"; restore: \"$restore_said\", no file written -- and restored by the new"
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
    answered "the old primary's reads of its own history" "$want"
    "$OLD/fennec" key "$C/old.key" >"$C/old-key.log" 2>&1 || fail "the old \`fennec key\` made no key"

    # c1: the old `fennec backup`, plain and sealed, opened by the new.
    "$OLD/fennec" backup "$P" "$C/backup.fennec" --token r >"$C/c1-backup.log" 2>&1 ||
        fail "the old backup failed: $(tr '\n' ' ' <"$C/c1-backup.log")"
    "$OLD/fennec" backup "$P" "$C/backup.sealed" --token r --key-file "$C/old.key" \
        >"$C/c1-sealed.log" 2>&1 || fail "the old sealed backup failed: $(tr '\n' ' ' <"$C/c1-sealed.log")"
    opened c1-open "$NEW" "$C/backup.fennec"
    state_holds "$want" "$OPENED" || why="its plain backup, opened by the new server: $(first_difference "$want" "$OPENED")"
    v=$(sealed_version "$C/backup.sealed")
    [ -n "$why" ] || [ "$v" = 1 ] || why="its sealed backup is no version 1 file (version '$v')"
    if [ -z "$why" ]; then
        archived "$C/backup.sealed" "$NEW" "$C/c1-sealed.fennec" --key-file "$C/old.key"
        state_holds "$want" "$OPENED" || why="its sealed backup, restored by the new \`fennec\`: $(first_difference "$want" "$OPENED")"
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
    state_holds "$want" "$(state "$N")" || why="the new server's reads of it: $(first_difference "$want" "$(state "$N")")"
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
        state_holds "$OPENED" "$after" || why="opened by the old server again: $(first_difference "$after" "$OPENED")"
    fi
    if [ -z "$why" ]; then
        held c2 "a file the old server wrote, opened and written by the new, verified and typed (\`fennec types\`) by both and opened by the old again with the new writes"
    else
        differs c2 "a file the old server wrote, then the new: $why"
    fi

    # c3: the new `fennec backup`, plain, opened by the old server; sealed
    # (version 3), refused by the old `fennec restore` in RELEASING.md's words.
    why=
    server new-primary "$NEW" --file "$C/n.fennec" --http-token t --replication-token r
    local NN=$ADDR
    SNAP=()
    history "$NN" 0 "${#HISTORY[@]}"
    want=${SNAP[${#HISTORY[@]}]}
    answered "the new primary's reads of its own history" "$want"
    "$NEW/fennec" key "$C/new.key" >"$C/new-key.log" 2>&1 || fail "the new \`fennec key\` made no key"
    "$NEW/fennec" backup "$NN" "$C/new-backup.fennec" --token r >"$C/c3-backup.log" 2>&1 ||
        fail "the new backup failed: $(tr '\n' ' ' <"$C/c3-backup.log")"
    "$NEW/fennec" backup "$NN" "$C/new-backup.sealed" --token r --key-file "$C/new.key" \
        >"$C/c3-sealed.log" 2>&1 || fail "the new sealed backup failed: $(tr '\n' ' ' <"$C/c3-sealed.log")"
    opened c3-open "$OLD" "$C/new-backup.fennec"
    state_holds "$OPENED" "$want" || why="its plain backup, opened by the old server: $(first_difference "$want" "$OPENED")"
    v=$(sealed_version "$C/new-backup.sealed")
    [ -n "$why" ] || [ "$v" = 3 ] || why="its sealed backup is no version 3 file (version '$v')"
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
        held c3 "the new \`fennec backup\` opened by the old server; sealed (version 3), refused by the old restore as promised, in words naming no version: \"$said_\", no file written"
    else
        differs c3 "the new \`fennec backup\`: $why"
    fi
}

# --- d: the router ----------------------------------------------------------------

# The address node N said it listens on.
node_at() { cat "$C/$1.addr"; }

# insecure_under ROUTER_BIN NODE_BIN: `--insecure` where the router is the
# old release's and the nodes this tree's. They ask no data token, and such
# a node refuses what a router from before forwards it, from loopback too
# -- its one mark is the one a router listening past loopback sends -- unless
# it runs `--insecure`: what the upgrade order, routers first, never meets.
insecure_under() {
    if [ "$1" = "$OLD" ] && [ "$2" = "$NEW" ]; then echo --insecure; fi
}

# router_case NAME ROUTER_BIN NODE_BIN: a tenant created by ROUTER_BIN's
# router on NODE_BIN's nodes, written, copied to its replica, moved and
# deleted, each step working or refused in words.
router_case() {
    local name=$1 rbin=$2 nbin=$3 n code insecure
    insecure=$(insecure_under "$rbin" "$nbin")
    for n in n1 n2 n3; do
        server "$n" "$nbin" --dir "$C/$n" --admin-token "a$n" --replication-token r --sync always \
            ${insecure:+"$insecure"}
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
    answered "the reads of the tenant written through the router" "$want"
    admin "$RT" GET /_shard/tenants/acme >/dev/null
    rep=$(jq -r '.replica // .replica_on // empty' "$C/admin.out")
    [ -n "$rep" ] || fail "the tenant was given no replica: $(cat "$C/admin.out")"
    until_state "$(node_at "$rep")/t/acme" "$want" ||
        fail "the replica on $rep is not the tenant: $(first_difference "$want" "$(state "$(node_at "$rep")/t/acme")")"
    local to=
    for n in n2 n3; do
        if [ "$n" != "$rep" ]; then to=$n; fi
    done
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
    answered "the moved tenant's reads" "$want"
    admin "$RT" GET /_shard/tenants/acme >/dev/null
    rep=$(jq -r '.replica // .replica_on // empty' "$C/admin.out")
    [ -n "$rep" ] || fail "the moved tenant has no replica: $(cat "$C/admin.out")"
    until_state "$(node_at "$rep")/t/acme" "$want" ||
        fail "the moved tenant's replica on $rep is not the tenant: $(first_difference "$want" "$(state "$(node_at "$rep")/t/acme")")"
    code=$(admin "$RT" DELETE /_shard/tenants/acme)
    [ "$code" = 204 ] || fail "the delete answered $code: $(cat "$C/admin.out")"
    for n in n1 n2 n3; do
        if [ -e "$C/$n/acme.fennec" ]; then fail "the delete left $n's acme.fennec"; fi
    done
    held "$name" "a tenant created on a node with a replica on another, its history written through the router, copied, moved to a third node with its replica following, and deleted from every node"
}

case_d1() { router_case d1 "$OLD" "$NEW"; }
case_d2() { router_case d2 "$NEW" "$OLD"; }

# --- e: a rolling upgrade under writes ---------------------------------------------

# New acknowledgements each step waits for before the next, so every step
# meets writes; the writer's pause between two writes, and after a write
# not acknowledged (a refusal is no reason to spin); and the writes it may
# send at most, whose end before the case's is the case's failure.
STEP_ACKS=50
WRITER_PAUSE=0.02
REFUSED_PAUSE=0.1
WRITER_CAP=100000

# The runbook's wait between two nodes' restarts: the standby's own
# /t/<tenant>/_replication/status connected with `behind` 0, and the
# primary's listing a stream whose `sent` is its `durable`.
standby_caught_up() {
    local s= p=
    for _ in $(seq 1 1200); do
        s=$(status "$E_N2/t/acme" 2>/dev/null || true)
        p=$(status "$E_N1/t/acme" 2>/dev/null || true)
        if echo "$s" | jq -e '.connected == true and .behind == 0' >/dev/null 2>&1 &&
            echo "$p" | jq -e '.durable as $d | [.replicas[] | select(.sent == $d)] | length >= 1' >/dev/null 2>&1; then
            return 0
        fi
        sleep 0.05
    done
    fail "the standby did not catch up in 60 s ($1): the standby's status $(echo "$s" | cut -c1-200); the primary's $(echo "$p" | cut -c1-200)"
}

case_e() {
    local n1 n2 rt n1p n2p rtp p code
    server n1 "$OLD" --dir "$C/n1" --admin-token a1 --replication-token r --sync always
    n1=$ADDR n1p=$PID
    server n2 "$OLD" --dir "$C/n2" --admin-token a2 --replication-token r --replica-of "$n1"
    n2=$ADDR n2p=$PID
    router router "$OLD" --directory "$C/router.fennec" --token root
    rt=$ADDR rtp=$PID
    code=$(admin "$rt" PUT /_shard/nodes/n2 "{\"addr\":\"${n2#http://}\",\"token\":\"a2\"}")
    case $code in 2??) ;; *) fail "the router took no node n2 ($code): $(cat "$C/admin.out")" ;; esac
    code=$(admin "$rt" PUT /_shard/nodes/n1 "{\"addr\":\"${n1#http://}\",\"token\":\"a1\",\"standby\":\"n2\"}")
    case $code in 2??) ;; *) fail "the router took no node n1 ($code): $(cat "$C/admin.out")" ;; esac
    code=$(admin "$rt" PUT /_shard/tenants/acme '{"node":"n1"}')
    case $code in 2??) ;; *) fail "the create was refused ($code): $(cat "$C/admin.out")" ;; esac
    # @hash, not @unique: a seq landing twice -- a write run twice by a
    # resend -- must show as two rows, not as a refusal.
    w "$rt/t/acme" "create collection seqs (seq int @hash)"
    echo "$rt" >"$C/router.addr"
    E_N1=$n1 E_N2=$n2

    # One writer, each write a new seq, sent through whatever router.addr
    # names, classed by its answer: `ack` a 2xx (durable on the primary,
    # --sync always), `unsent` curl's exit 7 (no connection) or the router's
    # 503 saying the request was not sent (`Fennec-Unsent`), `unknown`
    # anything else (a timeout, a 502), which may have run.
    (
        trap - ERR
        set +eEu
        trap 'exit 0' TERM
        i=0
        while [ ! -e "$C/writer.stop" ] && [ "$i" -lt "$WRITER_CAP" ]; do
            i=$((i + 1))
            rm -f "$C/writer.body" "$C/writer.head"
            code=$(curl -sS --max-time 10 -o "$C/writer.body" -D "$C/writer.head" -w '%{http_code}' \
                -H 'Authorization: Bearer t' -H 'Content-Type: application/json' \
                -d "{\"query\":\"put seqs {seq: $i}\"}" "$(cat "$C/router.addr")/t/acme/query" 2>/dev/null)
            rc=$?
            body=$(head -c 160 "$C/writer.body" 2>/dev/null | tr '\n' ' ')
            if [ "$rc" = 0 ] && [ "${code#2}" != "$code" ] && grep -q '"affected":1' <<<"$body"; then
                echo "$i ack"
            elif [ "$rc" = 7 ]; then
                echo "$i unsent: no connection (curl exit 7)"
                sleep "$REFUSED_PAUSE"
            elif [ "$code" = 503 ] && { grep -qi '^fennec-unsent: *1' "$C/writer.head" 2>/dev/null ||
                grep -q 'the request was not sent' <<<"$body"; }; then
                echo "$i unsent: $body"
                sleep "$REFUSED_PAUSE"
            else
                echo "$i unknown: curl exit $rc, status $code, $body"
                sleep "$REFUSED_PAUSE"
            fi
            sleep "$WRITER_PAUSE"
        done >"$C/writer.out"
        echo done >>"$C/writer.out"
    ) &
    local writer=$!
    echo "$writer" >>"$WORK/pids"
    echo "$writer" >>"$C/pids"
    acked() { grep -c ' ack$' "$C/writer.out" 2>/dev/null || true; }
    # wait_acked WHAT: STEP_ACKS writes acknowledged after WHAT; a writer
    # gone before them fails the case, as a step that let none through does.
    wait_acked() {
        local before now
        before=$(acked)
        for _ in $(seq 1 1200); do
            now=$(acked)
            [ "$now" -ge $((before + STEP_ACKS)) ] && return 0
            kill -0 "$writer" 2>/dev/null ||
                fail "the writer is gone after $now acknowledged writes, waiting for $STEP_ACKS after $1: $(tail -2 "$C/writer.out" | tr '\n' ' ')"
            sleep 0.05
        done
        fail "$((now - before)) of $STEP_ACKS writes acknowledged in 60 s after $1: $(tail -3 "$C/writer.out" | tr '\n' ' ')"
    }
    wait_acked "the start"

    # 1. Archivers: none here. 2. The router: a router is found by its
    # tunnel's name, not its port, so it comes back on a port of its own
    # and the writer is told it.
    stop "$rtp"
    router router-new "$NEW" --directory "$C/router.fennec" --token root
    rtp=$PID rt=$ADDR
    echo "$rt" >"$C/router.addr.new"
    mv -f "$C/router.addr.new" "$C/router.addr"
    wait_acked "the router's upgrade"

    # 3. The nodes, one at a time, the standby's first, back on a port of
    # its own, its new address told to the router.
    standby_caught_up "before the standby's node"
    stop "$n2p"
    server n2-new "$NEW" --dir "$C/n2" --admin-token a2 --replication-token r --replica-of "$n1"
    n2p=$PID n2=$ADDR E_N2=$ADDR
    code=$(admin "$rt" PUT /_shard/nodes/n2 "{\"addr\":\"${n2#http://}\",\"token\":\"a2\"}")
    case $code in 2??) ;; *) fail "the router took no new address for n2 ($code): $(cat "$C/admin.out")" ;; esac
    standby_caught_up "after the standby's node's upgrade"
    wait_acked "the standby's node's upgrade"

    # The primary's node, back at its own address: its standby follows it
    # by the address it was started with (--replica-of), as a machine keeps
    # its address across a restart. Its tenant takes no write meanwhile:
    # the router answers "was not sent" (automatic failover is off unless
    # --auto-failover asks).
    standby_caught_up "before the primary's node"
    p=${n1##*:}
    stop "$n1p"
    # On Linux the port it freed is in the ephemeral range, where another
    # socket can take it before the node is back. The case keeps the address
    # (the standby follows it there) and waits, bounded, for it to be free,
    # starting the node again each time it exits saying it is in use: the
    # case is never rerun, and the wait is said on its line.
    local rebound=0 up_rc=1
    for _ in $(seq 1 100); do
        spawn "$C/n1-new.log" "$NEW/fennec-server" --http "127.0.0.1:$p" --dir "$C/n1" --admin-token a1 \
            --replication-token r --sync always
        n1p=$SPAWNED
        up_rc=0
        up "$C/n1-new.log" "$n1p" >/dev/null || up_rc=$?
        [ "$up_rc" = 0 ] && break
        if [ "$up_rc" = 1 ] && grep -qi 'address already in use\|address in use' "$C/n1-new.log"; then
            rebound=$((rebound + 1))
            wait "$n1p" 2>/dev/null || true
            sleep 0.3
            continue
        fi
        break
    done
    [ "$up_rc" = 0 ] ||
        fail "the primary's node did not come back at 127.0.0.1:$p after $rebound waits for the port (status $up_rc): $(tail -3 "$C/n1-new.log" | tr '\n' ' ' | bare)"
    wait_acked "the primary's node's upgrade"
    touch "$C/writer.stop"
    for _ in $(seq 1 600); do
        kill -0 "$writer" 2>/dev/null || break
        sleep 0.05
    done
    if kill -0 "$writer" 2>/dev/null; then fail "the writer did not stop in 30 s"; fi
    grep -qx done "$C/writer.out" || fail "the writer ended without its last line: $(tail -2 "$C/writer.out" | tr '\n' ' ')"
    standby_caught_up "the end"

    # Each class held to the rows of the primary and of the standby.
    local sent ok unsent unknown landed_unknown
    # awk, not grep: a class with no write is no failure.
    awk '$2 == "ack" { print $1 }' "$C/writer.out" | sort >"$C/acked.seqs"
    awk '$2 == "unsent:" { print $1 }' "$C/writer.out" | sort >"$C/unsent.seqs"
    awk '$2 == "unknown:" { print $1 }' "$C/writer.out" | sort >"$C/unknown.seqs"
    sent=$(grep -c '^[0-9]' "$C/writer.out" || true)
    ok=$(wc -l <"$C/acked.seqs" | tr -d ' ')
    unsent=$(wc -l <"$C/unsent.seqs" | tr -d ' ')
    unknown=$(wc -l <"$C/unknown.seqs" | tr -d ' ')
    [ "$((ok + unsent + unknown))" = "$sent" ] ||
        fail "the writer's lines do not class every write: $ok + $unsent + $unknown of $sent"
    local where count
    for where in primary standby; do
        local at=$n1
        [ "$where" = standby ] && at=$n2
        q "$at/t/acme" "get seqs select seq" | jq -r '.[].seq' | sort >"$C/$where.all" ||
            fail "the $where's rows were not read: $(q "$at/t/acme" "get seqs count" | cut -c1-200)"
        count=$(q "$at/t/acme" "get seqs count" | jq -r '.[0].count')
        [ "$(wc -l <"$C/$where.all" | tr -d ' ')" = "$count" ] ||
            fail "the $where's rows read are not its count, $count"
        [ -z "$(uniq -d "$C/$where.all")" ] ||
            fail "a seq landed twice on the $where: $(uniq -d "$C/$where.all" | head -5 | tr '\n' ' ')"
        uniq "$C/$where.all" >"$C/$where.seqs"
        [ -z "$(comm -23 "$C/acked.seqs" "$C/$where.seqs")" ] ||
            fail "$(comm -23 "$C/acked.seqs" "$C/$where.seqs" | wc -l | tr -d ' ') of $ok acknowledged writes are not on the $where: $(comm -23 "$C/acked.seqs" "$C/$where.seqs" | head -5 | tr '\n' ' ')"
        [ -z "$(comm -12 "$C/unsent.seqs" "$C/$where.seqs")" ] ||
            fail "writes the router said it did not send landed on the $where: $(comm -12 "$C/unsent.seqs" "$C/$where.seqs" | head -5 | tr '\n' ' ')"
    done
    cmp -s "$C/primary.seqs" "$C/standby.seqs" ||
        fail "the standby, caught up, does not hold the primary's rows: $(diff "$C/primary.seqs" "$C/standby.seqs" | head -4 | tr '\n' ' ')"
    local note=
    [ "$rebound" = 0 ] || note=" (the primary's node's port was taken meanwhile: waited for, $rebound times)"
    landed_unknown=$(comm -12 "$C/unknown.seqs" "$C/primary.seqs" | wc -l | tr -d ' ')
    held e "the router, the standby's node and the primary's node upgraded in turn under one writer, $STEP_ACKS writes acknowledged after each and the standby caught up (its status connected, \`behind\` 0; the primary's \`sent\` its \`durable\`) before each node: $sent writes sent, $ok acknowledged (every one on the primary and the standby), $unsent not sent (none landed), $unknown unknown ($landed_unknown landed), no seq twice. Each restart a graceful TERM: durability under a kill is the power-cut and chaos rigs', not this case's$note"
}

# --- f: a GitHub hook kept with `key delivery` ---------------------------------------

# RELEASING.md: a single server whose hooks file has a `github` line saying
# `key delivery` refuses to start on the release, exit 2, naming the line;
# a tenant's `github` hook 0.2.0 saved with `key delivery` is kept, its
# events answered 409 until it is set again with `key body`, its statements
# not armed, and the node says so as it starts. On Linux the old node is
# musl, its sidecar naming the tenant's file with no birth time, and the new
# a glibc build reading one: f2 holds the set as the tenant's across the two
# (docs/design/tenants.md, a birth time held where both sides name one),
# which the first run on main found put away, every event of it 401.
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
    local rc said_
    refused_start "$C/new-single.log" "$NEW/fennec-server" --file "$C/s.fennec" --http 127.0.0.1:0 \
        --http-token t --hooks "$C/hooks"
    rc=$REFUSED_RC
    said_=$(grep -m1 'hooks file line 1: `key delivery` is refused' "$C/new-single.log" | bare | cut -c1-120 || true)
    if [ "$rc" = 2 ] && [ -n "$said_" ]; then
        held f1 "a hooks file's \`github\` line saying \`key delivery\`: $OLD_SAYS starts on it, the new server refuses it as promised, exit 2: \"$said_...\""
    else
        differs f1 "a hooks file's \`github\` line saying \`key delivery\`: the new server exited $rc, saying $(tr '\n' ' ' <"$C/new-single.log" | bare | cut -c1-300)"
    fi

    # f2: a tenant's hook, set on the old node; the router and then the node
    # upgraded in turn, each on a port of its own, the router told the
    # node's new address.
    server n1 "$OLD" --dir "$C/n1" --admin-token a1
    local n1=$ADDR n1p=$PID
    router router "$OLD" --directory "$C/router.fennec" --token root
    local rt=$ADDR rtp=$PID code out
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

    stop "$rtp"
    router router-new "$NEW" --directory "$C/router.fennec" --token root
    rtp=$PID rt=$ADDR
    stop "$n1p"
    server n1-new "$NEW" --dir "$C/n1" --admin-token a1
    n1p=$PID n1=$ADDR
    code=$(admin "$rt" PUT /_shard/nodes/n1 "{\"addr\":\"${n1#http://}\",\"token\":\"a1\"}")
    case $code in 2??) ;; *) fail "the new router took no new address for n1 ($code): $(cat "$C/admin.out")" ;; esac

    local why= refused listing armed
    refused=$(event "$rt/t/acme/_hooks/gh" '{"after":"bbb","ref":"refs/heads/main"}' d2 || true)
    case $refused in
    *'set again with `key body`'*' 409') ;;
    *) why="its verified event was answered $refused" ;;
    esac
    [ -n "$why" ] || [ "$(q "$rt/t/acme" "get pushes count" | jq -r '.[0].count' 2>/dev/null || true)" = 1 ] ||
        why="the refused event wrote: $(q "$rt/t/acme" "get pushes")"
    [ -n "$why" ] || grep -q 'was set with `key delivery`' "$C/n1-new.log" ||
        why="the upgraded node said nothing of the hook as it started: $(tail -3 "$C/n1-new.log" | tr '\n' ' ')"
    listing=$(curl -sS -H 'Authorization: Bearer root' "$rt/_shard/tenants/acme/hooks" || true)
    [ -n "$why" ] || echo "$listing" | jq -e '.hooks[0].key == "delivery"' >/dev/null 2>&1 ||
        why="its listing does not say \"key\":\"delivery\": $listing"
    armed=$(q "$rt/t/acme" "statements" | jq -c '[.[] | select(.name == "pushed") | .armed]' 2>/dev/null ||
        echo "a listing that is no list: $(q "$rt/t/acme" statements | cut -c1-200)")
    [ -n "$why" ] || [ "$armed" = '[false]' ] || why="its statement's \`armed\` is $armed"
    if [ -z "$why" ]; then
        code=$(admin "$rt" PUT /_shard/tenants/acme/hooks/gh "{\"scheme\":\"github\",\"secrets\":[\"$GH_SECRET\"],\"key\":\"body\"}")
        case $code in 2??) ;; *) why="setting it again with \`key body\` was refused ($code): $(cat "$C/admin.out")" ;; esac
    fi
    if [ -z "$why" ]; then
        out=$(event "$rt/t/acme/_hooks/gh" '{"after":"bbb","ref":"refs/heads/main"}' d3 || true)
        case $out in *' 200') ;; *) why="the redelivery after \`key body\` was answered $out" ;; esac
        [ -n "$why" ] || [ "$(q "$rt/t/acme" "get pushes count" | jq -r '.[0].count' 2>/dev/null || true)" = 2 ] ||
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
    local name=$1 rbin=$2 nbin=$3 certs=$ROOT/crates/fennec-http/tests/certs port code insecure
    insecure=$(insecure_under "$rbin" "$nbin")
    spawn "$C/node.log" "$nbin/fennec-server" --dir "$C/node" --http 127.0.0.1:0 --admin-token atls \
        --tls-cert "$certs/server.pem" --tls-key "$certs/server.key" ${insecure:+"$insecure"}
    local np=$SPAWNED
    for _ in $(seq 1 600); do
        grep -q 'listening on: https://' "$C/node.log" && break
        kill -0 "$np" 2>/dev/null || fail "the node serving HTTPS exited: $(tail -2 "$C/node.log")"
        sleep 0.05
    done
    # awk, not a pipe of greps: no line is no failure of the pipeline, so the
    # message below is reached.
    port=$(awk '/listening on: https:\/\//{ match($0, /https:\/\/[^ ]*/); print substr($0, RSTART, RLENGTH); exit }' "$C/node.log")
    port=${port##*:}
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

# Every case, and the parts it owes: each prints exactly one line a part,
# and the run is held to every part of the cases it ran.
CASES=(
    "a1:a1"
    "a2:a2"
    "a3:a3"
    "a3b:a3b"
    "a4:a4"
    "b:b1 b2 b3 b4"
    "c:c1 c2 c3"
    "d1:d1"
    "d2:d2"
    "e:e"
    "f:f1 f2"
    "g1:g1"
    "g2:g2"
)

# consts ROOT REGEX: every `const NAME: u8` of ROOT's crates/fennec-core/src
# whose NAME REGEX matches, in any file and of any visibility (`pub(in ..)`
# among them), one a line.
consts() {
    (cd "$1/$CORE_SRC" && grep -rhE --include='*.rs' \
        "^[[:space:]]*(pub(\([^)]*\))?[[:space:]]+)?const[[:space:]]+$2[[:space:]]*:[[:space:]]*u8" .) |
        sed -E 's/^.*const[[:space:]]+([A-Z0-9_]+).*$/\1/' | sort -u
}

# arm_codes MODE FILE IMPL FN: the integers in `fn FN` of `impl IMPL` in FILE
# (a free function where IMPL is empty), sorted, one a line: MODE arms, the
# patterns of its match arms (`1 | 8 =>`); MODE push, the numbers an
# `out.push(..)` of it writes. Empty where the function is not found, which
# check_table refuses.
arm_codes() {
    awk -v mode="$1" -v impl="$3" -v fn="$4" '
        /^impl / { cur = $2; sub(/[^A-Za-z0-9_].*$/, "", cur) }
        /^}/ { cur = "" }
        !inside && $0 ~ ("fn " fn "\\(") && cur == impl {
            inside = 1
            ind = $0
            sub(/[^ ].*$/, "", ind)
            next
        }
        inside && $0 == ind "}" { inside = 0 }
        inside {
            line = $0
            sub(/\/\/.*/, "", line)
            if (mode == "arms" && line ~ /^[ ]*[0-9]+([ ]*\|[ ]*[0-9]+)*[ ]*=>/) {
                sub(/=>.*$/, "", line)
                gsub(/ /, "", line)
                n = split(line, parts, "|")
                for (i = 1; i <= n; i++) print parts[i]
            } else if (mode == "push" && line ~ /out\.push\(/) {
                gsub(/[^0-9]+/, " ", line)
                n = split(line, parts, " ")
                for (i = 1; i <= n; i++) print parts[i]
            }
        }' "$2" | sort -u
}

# stream_tags ROOT written|read: the tags the replication stream's primary
# writes (`b'X',` as an argument of `message`) or its replica matches
# (`b'X' =>`), sorted, one a line.
stream_tags() {
    local f=$1/crates/fennec-http/src/replication.rs
    case $2 in
    written) grep -oE "b'[A-Z]'," "$f" | sed -E "s/^b'(.).*/\1/" | sort -u ;;
    read) grep -oE "b'[A-Z]'( if .*)? =>" "$f" | sed -E "s/^b'(.).*/\1/" | sort -u ;;
    esac
}

# same_list WHAT A B: two lists (a line a code) are one; says what is in one
# and not the other. 1 where they differ.
same_list() {
    local only rc=0
    only=$(comm -23 <(echo "$2") <(echo "$3") | tr '\n' ' ')
    if [ -n "$only" ]; then
        echo "compat-matrix: $1: held by its first side and not its second: $only"
        rc=1
    fi
    only=$(comm -13 <(echo "$2") <(echo "$3") | tr '\n' ' ')
    if [ -n "$only" ]; then
        echo "compat-matrix: $1: held by its second side and not its first: $only"
        rc=1
    fi
    return $rc
}

# check_table LABEL CODES ENTRY...: CODES, a line each, is every code of a
# numbered space the tree has; each ENTRY is `CODE|a write of HISTORY it
# begins` or `CODE|- why none is needed`. Every code has its entry, no entry
# names a code the tree has not, none is twice, every write begins one of
# HISTORY's. Says what is wrong; 1 where anything is.
check_table() {
    local label=$1 codes=$2 bad=0 entry found how h f code dups
    shift 2
    if [ -z "$codes" ]; then
        echo "compat-matrix: $label: no code found in the tree's sources: what the check reads has moved"
        return 1
    fi
    for code in $codes; do
        found=
        for entry in "$@"; do
            if [ "${entry%%|*}" = "$code" ]; then found=$entry; fi
        done
        if [ -z "$found" ]; then
            echo "compat-matrix: $code has no line in $label: add a write making it to HISTORY and name it, or say why none is needed"
            bad=1
            continue
        fi
        how=${found#*|}
        case $how in
        '- '?*) ;;
        *)
            h=
            for f in "${HISTORY[@]}"; do
                case $f in "$how"*) h=1 ;; esac
            done
            if [ -z "$h" ]; then
                echo "compat-matrix: $label gives $code the write \`$how\`, which no write of HISTORY begins with"
                bad=1
            fi
            ;;
        esac
    done
    for entry in "$@"; do
        if ! grep -qxF "${entry%%|*}" <<<"$codes"; then
            echo "compat-matrix: $label names ${entry%%|*}, which the tree does not have"
            bad=1
        fi
    done
    dups=$(for entry in "$@"; do echo "${entry%%|*}"; done | sort | uniq -d)
    if [ -n "$dups" ]; then
        echo "compat-matrix: $label names more than once: $(echo "$dups" | tr '\n' ' ')"
        bad=1
    fi
    return $bad
}

# check_tables ROOT: every table held to ROOT's sources, and UNKNOWN_TO_OLD
# to HISTORY. Says what is wrong; 1 where anything is.
check_tables() {
    local root=$1 bad=0 core=$1/$CORE_SRC enc dec w r entry h f codes kind code
    check_table FORMAT "$(consts "$root" '(REC|TAG|OP)_[A-Z0-9_]+')" "${FORMAT[@]}" || bad=1
    check_table GRAPHS "$(consts "$root" 'GRAPH_VERSION[A-Z0-9_]*')" "${GRAPHS[@]}" || bad=1
    # The index kinds: what `encode_index` writes is what `decode_index` reads.
    enc=$(arm_codes push "$core/schema.rs" "" encode_index)
    dec=$(arm_codes arms "$core/schema.rs" "" decode_index)
    same_list "an index kind written by encode_index and read by decode_index" "$enc" "$dec" || bad=1
    check_table INDEX_KINDS "$dec" "${INDEX_KINDS[@]}" || bad=1
    # A metric, a quantization and a collation, each as its `from_code` reads it.
    codes=
    for kind in "Metric $core/schema.rs metric" "Quant $core/schema.rs quant" "Collation $core/collate.rs collation"; do
        set -- $kind
        for code in $(arm_codes arms "$2" "$1" from_code); do
            codes="$codes$3:$code
"
        done
    done
    check_table SMALL_CODES "$(printf '%s' "$codes" | grep . || true)" "${SMALL_CODES[@]}" || bad=1
    # The replication stream's tags: written where the primary sends, read
    # where the replica matches.
    w=$(stream_tags "$root" written)
    r=$(stream_tags "$root" read)
    same_list "a message tag the replication stream writes and reads" "$w" "$r" || bad=1
    check_table STREAM "$r" "${STREAM[@]}" || bad=1
    for entry in ${UNKNOWN_TO_OLD[@]+"${UNKNOWN_TO_OLD[@]}"}; do
        h=
        for f in "${HISTORY[@]}"; do
            if [ "$f" = "${entry%%|*}" ]; then h=1; fi
        done
        if [ -z "$h" ] || [ "${entry#*|}" = "$entry" ]; then
            echo "compat-matrix: UNKNOWN_TO_OLD's \`${entry%%|*}\` is no write of HISTORY, or has no stop words"
            bad=1
        fi
    done
    return $bad
}

# run_cases SPEC...: each SPEC `case:part ...`, case_<case> run in a
# subshell of its own; its lines printed, its logs' tails where it failed.
# 1 where any case failed or any part went unsaid.
run_cases() {
    local spec c parts p n line own rc failed=0 expected= missing= unfinished=
    : >"$WORK/all-lines"
    for spec in "$@"; do
        c=${spec%%:*}
        parts=${spec#*:}
        expected="$expected $parts"
        CASE=$c
        C=$WORK/$c
        mkdir -p "$C"
        : >"$C/pids"
        : >"$C/lines"
        rm -f "$C/lines.extra" "$C/finished"
        SNAP=()
        PSEQ=()
        (
            set -eEuo pipefail
            # Always written down, whatever the case printed before: a case
            # dying after its first part is a failure, not a pass.
            trap 'echo "$CASE FAILED: line $LINENO: \`$BASH_COMMAND\` failed (exit $?)" >>"$C/lines"' ERR
            # shellcheck disable=SC2046 # one pid a word
            trap 'stop $(cat "$C/pids")' EXIT
            "case_$c"
            # Written once the case has returned, and required beside its
            # parts: under bash 3.2 a subshell with an EXIT trap that dies on
            # `set -u` exits 0, so its status alone cannot be believed.
            echo "$c" >"$C/finished"
        ) >"$C/case.out" 2>&1
        rc=$?
        if [ "$rc" != 0 ] && ! grep -q "^$c FAILED: " "$C/lines"; then
            echo "$c FAILED: the case exited $rc" >>"$C/lines"
        fi
        if [ ! -f "$C/finished" ]; then
            unfinished="$unfinished $c"
            if ! grep -q "^$c FAILED: " "$C/lines"; then
                echo "$c FAILED: the case did not finish (its status, $rc, says nothing: see its output)" >>"$C/lines"
            fi
        fi
        # Each part said once, and nothing said of a part not its own.
        for p in $parts; do
            n=$(grep -Ec "^$p (ok|FAILED): " "$C/lines" || true)
            if [ "$n" = 0 ]; then
                echo "$p FAILED: case $c printed no line for it (it stopped before, or never said it)" >>"$C/lines"
            elif [ "$n" != 1 ]; then
                echo "$p FAILED: case $c printed $n lines for it" >>"$C/lines"
            fi
        done
        while read -r line; do
            own=
            for p in $parts; do
                if [ "${line%% *}" = "$p" ]; then own=1; fi
            done
            # The case's own lines are its failures.
            case $line in "$c FAILED: "*) own=1 ;; esac
            [ -n "$own" ] || echo "$c FAILED: it printed a line of no part of its own ($parts): $line" >>"$C/lines.extra"
        done <"$C/lines"
        if [ -s "$C/lines.extra" ]; then cat "$C/lines.extra" >>"$C/lines"; fi
        cat "$C/lines"
        cat "$C/lines" >>"$WORK/all-lines"
        if [ "$rc" != 0 ] || grep -q ' FAILED: ' "$C/lines"; then
            failed=$((failed + 1))
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
    # The end of the run held to the whole list.
    for p in $expected; do
        if ! grep -Eq "^$p ok: " "$WORK/all-lines" || grep -Eq "^$p FAILED: " "$WORK/all-lines"; then
            missing="$missing $p"
        fi
    done
    if [ "$failed" -gt 0 ] || [ -n "$missing" ] || [ -n "$unfinished" ]; then
        echo "compat-matrix: $failed case(s) failed; cases not finished:${unfinished:- none}; parts not held:${missing:- none (a case failed after its parts)}"
        return 1
    fi
    echo "compat-matrix: every part held:$expected"
}

# --self-test: the harness held to its own promises with toy cases and no
# binary -- a case dying after its first part, one printing a part too few,
# one dying in a command substitution (b2's path with no image), one
# printing a part not its own, one dying on an unset variable after every
# part, one exiting other than 0 with every part and its sentinel written
# (the status check alone), one failing a check in a helper used as an `if`
# condition, a state held by the old release's answer and not by a changed
# one, each failing the run -- and every table held to the tree, then to a
# tree with a code more or a code less.
self_test() {
    local out bad=0 toy
    case_t_good() { held t1 one; held t2 two; }
    case_t_dies() { held t3 one; false; held t4 two; }
    case_t_short() { held t5 one; }
    case_t_subst() {
        local v
        v=$(head -c 8 "$(first_image "$C")" | od -An -c)
        held t6 "$v"
    }
    case_t_alien() { held t7 one; held t8 "not its own"; }
    # Every part said, then an unset variable: bash 3.2 exits 0 from the
    # subshell with its EXIT trap; only the sentinel tells it did not finish.
    case_t_unset() {
        held t10 one
        echo "$never_set_variable"
        held t10b two
    }
    # Every part said and the sentinel written, then exit 3: only the
    # status check fails it.
    case_t_exit() {
        held t11 one
        echo "$CASE" >"$C/finished"
        exit 3
    }
    # A check in a helper used as an `if` condition: `set -e` does not act
    # there, so the helper's check is `|| fail`, which exits the case.
    case_t_cond() {
        t_check() { [ one = two ] || fail "the helper's check"; }
        if t_check; then :; fi
        held t12 one
    }
    # expect WHAT STATUS PATTERN SPEC...: run_cases on SPEC exits STATUS
    # (0, or 1 for any failure) and prints a line matching PATTERN.
    expect() {
        local what=$1 want=$2 pattern=$3 got
        shift 3
        out=$( (run_cases "$@") 2>/dev/null)
        got=$?
        [ "$got" -gt 1 ] && got=1
        if [ "$got" != "$want" ] || ! grep -Eq "$pattern" <<<"$out"; then
            echo "compat-matrix self-test FAILED: $what: exit $got, not $want, or no line /$pattern/: $(echo "$out" | tr '\n' ' ')"
            bad=1
        else
            echo "compat-matrix self-test ok: $what"
        fi
    }
    expect "a case printing every part passes" 0 '^compat-matrix: every part held: t1 t2$' "t_good:t1 t2"
    expect "a case dying after its first part fails, its unsaid part named" 1 '^t4 FAILED: case t_dies printed no line' \
        "t_good:t1 t2" "t_dies:t3 t4"
    expect "a case dying after its first part names its line" 1 '^t_dies FAILED: line [0-9]+: `false` failed' \
        "t_dies:t3 t4"
    expect "a case printing a part too few fails by the part's name" 1 'parts not held: t5b$' "t_short:t5 t5b"
    expect "a command failing in a substitution fails the case" 1 '^t_subst FAILED: ' "t_subst:t6"
    expect "a line of a part not its own fails the case" 1 '^t_alien FAILED: it printed a line of no part of its own' \
        "t_alien:t7"
    expect "an unset variable after every part fails the case" 1 '^t_unset FAILED: ' "t_unset:t10 t10b"
    expect "a case that did not finish is named in the run's last line" 1 'cases not finished: t_unset;' "t_unset:t10 t10b"
    expect "a status other than 0 fails a case whose parts and sentinel are all there" 1 '^t_exit FAILED: the case exited 3' \
        "t_exit:t11"
    expect "a failing check in a helper used as an if condition fails the case" 1 "^t_cond FAILED: the helper's check" \
        "t_cond:t12"
    # The old release's answer held by the new one's, and a changed key not.
    if state_holds '{"a":1,"b":[1,{"c":2}]}' '{"a":1,"b":[1,{"c":2,"later":3}],"also":4}' &&
        ! state_holds '{"a":1}' '{"a":2}' && ! state_holds '{"a":1,"b":2}' '{"a":1}' &&
        ! state_holds '"error"' '[]' && ! state_holds 'no answer to x' '"error"'; then
        echo "compat-matrix self-test ok: a state holds the older release's answer with keys added, not one changed or gone"
    else
        echo "compat-matrix self-test FAILED: state_holds"
        bad=1
    fi
    if out=$(check_tables "$ROOT"); then
        echo "compat-matrix self-test ok: every table names every code of the tree, and every line a code it has"
    else
        echo "compat-matrix self-test FAILED: the tables against the tree: $out"
        bad=1
    fi
    # Each table held both ways to a tree with a code more (a code with no
    # line) and to a table with a line more (a line with no code): the
    # tree's files copied, one mutated.
    toy=$WORK/toy
    for f in $CORE_SRC/engine.rs $CORE_SRC/engine/stored.rs $CORE_SRC/store.rs $CORE_SRC/codec.rs \
        $CORE_SRC/schema.rs $CORE_SRC/collate.rs $CORE_SRC/vector.rs crates/fennec-http/src/replication.rs; do
        mkdir -p "$toy/$(dirname "$f")"
        cp "$ROOT/$f" "$toy/$f"
    done
    mutant() {
        # mutant WHAT FILE AWK_PROGRAM NAME: FILE of the toy tree rewritten
        # by AWK_PROGRAM, which must change it, and check_tables must fail
        # naming NAME; the file is put back after.
        local what=$1 file=$toy/$2 prog=$3 name=$4
        cp "$file" "$file.orig"
        awk "$prog" "$file.orig" >"$file"
        if cmp -s "$file" "$file.orig"; then
            echo "compat-matrix self-test FAILED: $what: the mutation changed nothing (the source moved)"
            bad=1
        elif out=$(check_tables "$toy") || ! grep -q "$name" <<<"$out"; then
            echo "compat-matrix self-test FAILED: $what: passed, or failed without naming $name: $(echo "$out" | tr '\n' ' ' | cut -c1-300)"
            bad=1
        else
            echo "compat-matrix self-test ok: $what fails, naming $name"
        fi
        mv -f "$file.orig" "$file"
    }
    mutant "a record kind FORMAT does not name" $CORE_SRC/engine.rs '{ print } END { print "const REC_NEXT_ONE: u8 = 15;" }' \
        'REC_NEXT_ONE has no line in FORMAT'
    mutant "a type tag in a file of its own, its visibility pub(in ..)" $CORE_SRC/collate.rs \
        '{ print } END { print "pub(in crate::engine) const TAG_NEXT_ONE: u8 = 19;" }' 'TAG_NEXT_ONE has no line in FORMAT'
    mutant "a graph version GRAPHS does not name" $CORE_SRC/vector.rs \
        '{ print } /^const GRAPH_VERSION: u8/ { print "const GRAPH_VERSION_NEXT_ONE: u8 = 11;" }' 'GRAPH_VERSION_NEXT_ONE has no line in GRAPHS'
    mutant "an index kind decode_index reads and encode_index does not write" $CORE_SRC/schema.rs \
        '{ print } /^        0 => IndexKind::None,$/ { print "        12 => IndexKind::None," }' '12 has no line in INDEX_KINDS'
    mutant "an index kind encode_index writes and decode_index does not read" $CORE_SRC/schema.rs \
        '{ print } /^        IndexKind::Geo => out.push\(10\),$/ { print "        IndexKind::Geo => out.push(13)," }' 'not its second: 13'
    mutant "a metric code SMALL_CODES does not name" $CORE_SRC/schema.rs \
        '{ print } /^            2 => Metric::Dot,$/ { print "            3 => Metric::Dot," }' 'metric:3 has no line in SMALL_CODES'
    mutant "a quantization code SMALL_CODES does not name" $CORE_SRC/schema.rs \
        '{ print } /^            2 => Some\(Quant::Bit\),$/ { print "            3 => Some(Quant::Bit)," }' 'quant:3 has no line in SMALL_CODES'
    mutant "a collation code SMALL_CODES does not name" $CORE_SRC/collate.rs \
        '{ print } /^            2 => Some\(Collation::Root\),$/ { print "            3 => Some(Collation::Root)," }' 'collation:3 has no line in SMALL_CODES'
    mutant "a message tag the replica reads and the primary does not write" crates/fennec-http/src/replication.rs \
        '{ print } /^            b.E. => Message::End/ { print "            b'"'"'Z'"'"' => Message::End(String::new())," }' 'not its first: Z'
    for t in FORMAT GRAPHS INDEX_KINDS SMALL_CODES STREAM; do
        # The table with a line more, for a code the tree has not.
        if out=$(eval "$t=(\"\${$t[@]}\" \"ZZ_NEXT_ONE|put nothing\")" && check_tables "$ROOT"); then
            echo "compat-matrix self-test FAILED: a line of $t for no code of the tree passed"
            bad=1
        elif grep -q "$t names ZZ_NEXT_ONE" <<<"$out"; then
            echo "compat-matrix self-test ok: a line of $t naming no code of the tree fails"
        else
            echo "compat-matrix self-test FAILED: a $t line for no code failed in other words: $out"
            bad=1
        fi
    done
    return $bad
}

WORK=$(mktemp -d "${TMPDIR:-/tmp}/fennec-compat.XXXXXX")
: >"$WORK/pids"
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

if [ "${1:-}" = --self-test ]; then
    command -v jq >/dev/null || { echo "compat-matrix: needs jq" >&2; exit 2; }
    self_test
    exit $?
fi

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
OLD_SAYS=$(first_line "$OLD/fennec-server" --version)
NEW_SAYS=$(first_line "$NEW/fennec-server" --version)

# Both trees may say the same version (this tree's is bumped at a release),
# so the binaries are told apart by their paths and bytes.
digest() {
    if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi
}
for side in OLD NEW; do
    dir=$OLD
    [ "$side" = NEW ] && dir=$NEW
    for b in fennec-server fennec fennec-shard; do
        echo "$(echo "$side" | tr 'A-Z' 'a-z'): $dir/$b, sha256 $(digest "$dir/$b"), says \"$(first_line "$dir/$b" --version)\""
    done
done
for b in fennec-server fennec fennec-shard; do
    if [ "$(digest "$OLD/$b")" = "$(digest "$NEW/$b")" ]; then
        echo "compat-matrix: $OLD/$b and $NEW/$b are the same bytes: the matrix would hold a release to itself" >&2
        exit 2
    fi
done
check_tables "$ROOT" || exit 1

SPECS=()
for c in ${COMPAT_CASES:-a1 a2 a3 a3b a4 b c d1 d2 e f g1 g2}; do
    found=
    for spec in "${CASES[@]}"; do
        if [ "${spec%%:*}" = "$c" ]; then found=$spec; fi
    done
    [ -n "$found" ] || { echo "compat-matrix: no case $c (${CASES[*]})" >&2; exit 2; }
    SPECS+=("$found")
done
run_cases "${SPECS[@]}"
