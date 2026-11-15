#!/bin/sh
# The soak: fennec-server in a container whose --memory is under its file's
# size, for hours under YCSB A's updates, inserts and deletes, @ttl expiry,
# @text match, vector near, 64 subscriptions and a following replica, with
# a compact, a backup restored and verified, and a kill -9 of the server
# every 30 minutes -- its file verified at each restart and at the end
# (`ycsb soak`, crates/fennec-bench/src/soak.rs, says what each does).
#
#   nohup crates/fennec-bench/ycsb/run-soak.sh 250 > soak-250.log 2>&1 &
#   nohup crates/fennec-bench/ycsb/run-soak.sh always > soak-always.log 2>&1 &
#
# from the repository's root, on a machine doing nothing else, one sync mode
# a run: first the peak the mix reaches as fast as it is answered
# (PEAK_MINUTES), then the soak open loop at each of FRACTIONS of it, in
# turn: 8 hours each, 16 and a half for one sync mode as it comes. Each
# setting is an environment variable:
#
#   MINUTES=480 FRACTIONS="50 80" PEAK_MINUTES=4 RECORDS=1000000
#   MEMORY=512m THREADS=32 SUBSCRIPTIONS=64 KILL_EVERY=30
#   COMPACT_EVERY=20 BACKUP_EVERY=120 RUN=p6 QUIET_LOAD=4
#   OUT=crates/fennec-bench/results/soak.tsv
#
# A minute's line is appended to OUT (family `soak`) as the minute ends, so
# a run cut short keeps what it measured; `ycsb soak report OUT <run>`
# judges a run by the item's exit criterion, a line each, and is run at
# each soak's end. The runs are named <RUN>-s<sync>-peak and
# <RUN>-s<sync>-<fraction>.
#
# Refused before anything starts: --memory not under the file's size, a
# Docker VM without room for the server and its replica, less free disk
# than the run needs (printed), a load average over QUIET_LOAD (printed).
# Everything it starts is named p6-* and removed when it ends, however it
# ends: `kill <its pid>` stops it cleanly, the lines written so far kept.
set -eu
cd "$(dirname "$0")/../../.."

sync=${1:?usage: run-soak.sh 250|always}
case $sync in
250 | always) ;;
*)
    echo "usage: run-soak.sh 250|always" >&2
    exit 2
    ;;
esac
MINUTES=${MINUTES:-480}
FRACTIONS=${FRACTIONS:-50 80}
PEAK_MINUTES=${PEAK_MINUTES:-4}
RECORDS=${RECORDS:-1000000}
MEMORY=${MEMORY:-512m}
THREADS=${THREADS:-32}
SUBSCRIPTIONS=${SUBSCRIPTIONS:-64}
KILL_EVERY=${KILL_EVERY:-30}
COMPACT_EVERY=${COMPACT_EVERY:-20}
BACKUP_EVERY=${BACKUP_EVERY:-120}
RUN=${RUN:-p6}
QUIET_LOAD=${QUIET_LOAD:-4}
OUT=${OUT:-crates/fennec-bench/results/soak.tsv}

bytes() {
    echo "$1" | awk '{
        n = $0 + 0; u = tolower(substr($0, length($0)))
        if (u == "k") n *= 1024; else if (u == "m") n *= 1048576; else if (u == "g") n *= 1073741824
        printf "%.0f\n", n }'
}
load() {
    if [ "$(uname)" = Darwin ]; then
        sysctl -n vm.loadavg | awk '{print $2}'
    else
        cut -d' ' -f1 /proc/loadavg
    fi
}

limit=$(bytes "$MEMORY")
# A record is 1.1 KB in the file, which the updates grow until a compact.
live=$((RECORDS * 1100))
if [ "$limit" -ge "$live" ]; then
    echo "--memory $MEMORY is not under the file's $((live >> 20)) MB: raise RECORDS" >&2
    exit 2
fi
vm=$(docker info --format '{{.MemTotal}}')
if [ "$vm" -lt $((limit * 2 + (1 << 30))) ]; then
    echo "Docker's VM has $((vm >> 20)) MB: the server and its replica at --memory $MEMORY and a GB do not fit" >&2
    exit 2
fi
# The primary's file at 2.2 times its live bytes and a compact's image
# beside it, the replica's the same, a backup and its restore, the tools'
# build, the image and room to spare.
need=$(((live * 17 / 2) / 1024 + (4 << 20)))
free=$(df -k . | awk 'NR == 2 {print $4}')
echo "needs $((need >> 20)) GB of disk, $((free >> 20)) GB free; refused over a load of $QUIET_LOAD"
if [ "$free" -lt "$need" ]; then
    echo "not enough disk: $((need >> 20)) GB needed, $((free >> 20)) GB free" >&2
    exit 2
fi
if [ "$(load | awk -v q="$QUIET_LOAD" '{print ($1 > q)}')" = 1 ]; then
    echo "the load average is $(load), over $QUIET_LOAD: the machine is not quiet" >&2
    exit 2
fi

child=
# shellcheck disable=SC2329 # the EXIT trap's
cleanup() {
    [ -n "$child" ] && kill "$child" 2>/dev/null
    for c in p6-server p6-replica p6-look p6-verify p6-backup p6-build; do
        docker rm -f "$c" >/dev/null 2>&1
    done
    for v in p6-data p6-replica-data p6-backup-data p6-tools; do
        docker volume rm -f "$v" >/dev/null 2>&1
    done
    docker network rm p6-net >/dev/null 2>&1
    return 0
}
trap cleanup EXIT
trap 'exit 130' INT TERM
step() {
    "$@" &
    child=$!
    s=0
    wait "$child" || s=$?
    child=
    return "$s"
}

cargo=cargo
[ -x "$HOME/.cargo/bin/cargo" ] && cargo="$HOME/.cargo/bin/cargo"
awake=""
if command -v caffeinate >/dev/null 2>&1; then
    awake="caffeinate -i"
fi
mkdir -p "$(dirname "$OUT")"

echo "$(date '+%F %H:%M') the images and tools"
step docker build -q -t fennecdb-ycsb .
step crates/fennec-bench/ycsb/linux-tools.sh p6
step "$cargo" build --release -q -p fennec-bench --bin ycsb --no-default-features

common="--sync $sync --records $RECORDS --memory $MEMORY --threads $THREADS
    --subscriptions $SUBSCRIPTIONS --tools p6-tools --name p6 --out $OUT"

echo "$(date '+%F %H:%M') the peak, $PEAK_MINUTES minutes as fast as answered"
peaks="${TMPDIR:-/tmp}/run-soak-peak.$$"
# shellcheck disable=SC2086
step $awake ./target/release/ycsb soak $common --peak --minutes "$PEAK_MINUTES" \
    --run-id "$RUN-s$sync-peak" >"$peaks"
peak=$(awk '$1 == "peak" {print $2}' "$peaks")
rm -f "$peaks"
[ -n "$peak" ] || {
    echo "no peak measured" >&2
    exit 1
}
echo "$(date '+%F %H:%M') the peak is $peak operations a second"

status=0
for f in $FRACTIONS; do
    rate=$((peak * f / 100))
    run="$RUN-s$sync-$f"
    echo "$(date '+%F %H:%M') $run: $MINUTES minutes at $rate a second, $f% of the peak"
    # shellcheck disable=SC2086
    step $awake ./target/release/ycsb soak $common --rate "$rate" --label "$f" \
        --minutes "$MINUTES" --kill-every "$KILL_EVERY" --compact-every "$COMPACT_EVERY" \
        --backup-every "$BACKUP_EVERY" --run-id "$run" || status=1
    ./target/release/ycsb soak report "$OUT" "$run" || status=1
done
echo "$(date '+%F %H:%M') done"
exit "$status"
