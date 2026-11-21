#!/bin/sh
# The soak: fennec-server in a container whose --memory is under its file's
# size, for hours under YCSB A's updates, inserts and deletes, @ttl expiry,
# @text match, vector near, 64 subscriptions and a following replica, with
# a compact, a backup restored and verified, and a kill -9 of the server
# every 30 minutes -- its file verified at each restart and at the end
# (`ycsb soak`, crates/fennec-bench/src/soak.rs, says what each does).
#
#   crates/fennec-bench/ycsb/run-soak.sh --preflight
#   nohup crates/fennec-bench/ycsb/run-soak.sh 250 > soak-250.log 2>&1 &
#   nohup crates/fennec-bench/ycsb/run-soak.sh always > soak-always.log 2>&1 &
#   crates/fennec-bench/ycsb/run-soak.sh --self-test
#
# from the repository's root, on a machine doing nothing else, one sync mode
# a run: first the peak the mix reaches as fast as it is answered
# (PEAK_MINUTES), then the soak open loop at each of FRACTIONS of it, in
# turn: 8 hours each, 16 and a half for one sync mode as it comes. A
# fraction that failed its report ends the run there, unless KEEP_GOING=1.
# Each setting is an environment variable:
#
#   MINUTES=480 FRACTIONS="50 80" PEAK_MINUTES=4 RECORDS=1000000
#   MEMORY=512m THREADS=32 SUBSCRIPTIONS=64 KILL_EVERY=30
#   COMPACT_EVERY=20 BACKUP_EVERY=120 BACKUP_AT=50 KILL_IN_COMPACT_EVERY=90
#   LOSS_MS=650 (250; 0 under always) RESTART_MAX=120 RUN=p6 QUIET_LOAD=4
#   KEEP_GOING=0 OUT=crates/fennec-bench/results/soak.tsv
#
# A minute's line is appended to OUT (family `soak`) as the minute ends, so
# a run cut short keeps what it measured; `ycsb soak report OUT <run>`
# judges a run by the item's exit criterion, a line each, and the figures
# the two bounds are read against, and is run at each soak's end. The runs
# are named <RUN>-s<sync>-peak and <RUN>-s<sync>-<fraction>.
#
# `--preflight`, in a window where the network may be used, says what must
# be on this machine before a run -- the images by digest, the toolchain,
# the registry -- and builds the server's image from this tree, labelled
# with the tree's identity; every run checks the same first, the image by
# that label, and starts its servers from that image's id, failing in words
# where one is missing or the image was built from another tree: nothing is
# pulled, fetched or built while one runs. Refused before anything starts:
# --memory not under the file's size, a Docker VM without room for the
# server and its replica, less free disk than the run needs (printed), a
# load average over QUIET_LOAD (printed). Before the peak and each fraction
# the load must be down to QUIET_LOAD again (waited for, ENV_LOAD_WAIT s at
# most) and the host's and Docker's VM's disks must have what the run
# needs, and while one runs both disks are held to a floor and the soak to
# a line every ten minutes (ycsb/env-check.sh): a step the environment
# cannot take ends the run with a line `<sync>:<fraction>:env env_failed 1`
# -- the environment's failure, never a pass and never the server's -- and
# a soak silent ten minutes is stopped and marked `<sync>:<fraction>:hung`.
#
# Everything it starts is named after the run, its process id in the name
# (p6-250-<pid>-80-server), so that no run removes another's, and removed
# when it ends, however it ends: `kill <its pid>` stops it cleanly, the
# measurement under `caffeinate` too, the lines written so far kept -- but
# the volumes of a soak that failed a verify or counted a mismatch, whose
# names it prints. `--self-test` holds the trap and the guard to that with
# toys, no Docker.
set -eu
cd "$(dirname "$0")/../../.."
ENV_PREFIX=p6
. crates/fennec-bench/ycsb/env-check.sh
# Until the run's own cleanup takes over: what a bounded docker call was
# in, stopped, however the script ends (the pre-flight's build among them).
trap env_end EXIT
trap 'exit 130' INT TERM

case ${1:-} in
--self-test)
    env_self_test crates/fennec-bench/ycsb/run-soak.sh silent 250
    exit
    ;;
--preflight)
    env_preflight build
    exit
    ;;
esac
sync=${1:?usage: run-soak.sh 250|always|--preflight|--self-test}
case $sync in
250 | always) ;;
*)
    echo "usage: run-soak.sh 250|always|--preflight|--self-test" >&2
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
BACKUP_AT=${BACKUP_AT:-50}
KILL_IN_COMPACT_EVERY=${KILL_IN_COMPACT_EVERY:-90}
RESTART_MAX=${RESTART_MAX:-120}
RUN=${RUN:-p6}
QUIET_LOAD=${QUIET_LOAD:-4}
KEEP_GOING=${KEEP_GOING:-0}
OUT=${OUT:-crates/fennec-bench/results/soak.tsv}
# What the run is made of, each replaced by a toy in `--self-test`.
YCSB=${YCSB:-./target/release/ycsb}
LINUX_TOOLS=${LINUX_TOOLS:-crates/fennec-bench/ycsb/linux-tools.sh}
# rustup's cargo, never another on the PATH (CLAUDE.md).
cargo=${CARGO:-cargo}
[ -z "${CARGO:-}" ] && [ -x "$HOME/.cargo/bin/cargo" ] && cargo="$HOME/.cargo/bin/cargo"
case $sync in
always) LOSS_MS=${LOSS_MS:-0} ;;
*) LOSS_MS=${LOSS_MS:-650} ;;
esac
# The run's own names: its process id in each, so that a run never removes
# another's, an earlier one's kept volumes among them.
NAME="$RUN-$sync-$$"
ENV_PREFIX=$NAME

bytes() {
    echo "$1" | awk '{
        n = $0 + 0; u = tolower(substr($0, length($0)))
        if (u == "k") n *= 1024; else if (u == "m") n *= 1048576; else if (u == "g") n *= 1073741824
        printf "%.0f\n", n }'
}

limit=$(bytes "$MEMORY")
# A record is 1.1 KB in the file, which the updates grow until a compact.
live=$((RECORDS * 1100))
if [ "$limit" -ge "$live" ]; then
    echo "--memory $MEMORY is not under the file's $((live >> 20)) MB: raise RECORDS" >&2
    exit 2
fi
vm=$(env_within 60 "docker info" docker info --format '{{.MemTotal}}')
if [ "$vm" -lt $((limit * 2 + (1 << 30))) ]; then
    echo "Docker's VM has $((vm >> 20)) MB: the server and its replica at --memory $MEMORY and a GB do not fit" >&2
    exit 2
fi
# The primary's file at 2.2 times its live bytes and a compact's image
# beside it, the replica's the same, a backup and its restore, the tools'
# build, the image and room to spare.
need=$(((live * 17 / 2) / 1024 + (4 << 20)))
# A running step stops when either disk is under what a compact's image, a
# backup and its restore of the live records and a GB need: before a write
# fails for want of room, which would be the server's error.
ENV_FLOOR=$((live * 3 / 1024 + (1 << 20)))
ENV_GATE='' ENV_MARKER="${TMPDIR:-/tmp}/$NAME.env" ENV_PROGRESS="${TMPDIR:-/tmp}/$NAME.progress"
KEPT="${TMPDIR:-/tmp}/$NAME.kept"
free=$(df -k . | awk 'NR == 2 {print $4}')
echo "needs $((need >> 20)) GB of disk, $((free >> 20)) GB free; refused over a load of $QUIET_LOAD"
if [ "$free" -lt "$need" ]; then
    echo "not enough disk: $((need >> 20)) GB needed, $((free >> 20)) GB free" >&2
    exit 2
fi
if [ "$(env_load | awk -v q="$QUIET_LOAD" '{print ($1 > q)}')" = 1 ]; then
    echo "the load average is $(env_load), over $QUIET_LOAD: the machine is not quiet" >&2
    exit 2
fi

child=
# The soaks started, by name, for the cleanup.
names=
# shellcheck disable=SC2329 # the EXIT trap's
cleanup() {
    env_stop_child
    env_stop_guard
    rm -f "$ENV_MARKER" "$ENV_MARKER.hung" "$ENV_PROGRESS"
    for n in $names; do
        cs=
        for c in server replica server-look replica-look verify backup restored sampler; do
            cs="$cs $n-$c"
        done
        # shellcheck disable=SC2086
        env_within 120 "docker rm" docker rm -f $cs >/dev/null 2>&1
        for v in data replica-data backup-data; do
            if [ -f "$KEPT" ] && grep -qx "$n-$v" "$KEPT"; then
                echo "kept the volume $n-$v (a verify failed or a mismatch was counted): docker volume rm $n-$v once read" >&2
            else
                env_within 60 "docker volume rm" docker volume rm -f "$n-$v" >/dev/null 2>&1
            fi
        done
        env_within 60 "docker network rm" docker network rm "$n-net" >/dev/null 2>&1
    done
    env_within 120 "docker rm" docker rm -f "$NAME-build" "$NAME-df" >/dev/null 2>&1
    env_within 60 "docker volume rm" docker volume rm -f "$NAME-tools" >/dev/null 2>&1
    rm -f "$KEPT"
    env_end
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

awake=""
if command -v caffeinate >/dev/null 2>&1; then
    awake="caffeinate -i"
fi
mkdir -p "$(dirname "$OUT")"

env_preflight || {
    echo "the pre-flight failed: nothing was started" >&2
    exit 2
}
echo "$(date '+%F %H:%M') the tools"
step "$LINUX_TOOLS" "$NAME"
step "$cargo" build --release --offline -q -p fennec-bench --bin ycsb --no-default-features

# The servers start from the image the pre-flight found labelled with this
# tree, by its id: a build from another tree moving the tag meanwhile
# changes nothing a run starts.
common="--sync $sync --records $RECORDS --memory $MEMORY --threads $THREADS
    --subscriptions $SUBSCRIPTIONS --tools $NAME-tools --out $OUT --kept $KEPT
    --image $ENV_IMAGE
    --progress $ENV_PROGRESS --loss-ms $LOSS_MS --restart-max $RESTART_MAX"

# The environment failed run $1 (cell $2): marked so, and the run ends.
env_failed() {
    echo "$(date '+%F %H:%M') the environment's failure: $3" >&2
    "$YCSB" mark "$OUT" soak "$1" "$2" env_failed 1
    exit 3
}
# The guard stopped run $1 for printing nothing: marked so, the harness's
# or the server's to read, never the environment's.
hung() {
    echo "$(date '+%F %H:%M') the soak hung: $3" >&2
    "$YCSB" mark "$OUT" soak "$1" "$2" hung 1
    rm -f "$ENV_MARKER.hung"
}
# The names the guard stops a soak's containers by.
soak_names() {
    names="$names $1"
    ENV_VOLUME=$1-data
    ENV_KILL="$1-server $1-replica $1-server-look $1-replica-look $1-verify $1-backup $1-restored $1-sampler"
    : >"$ENV_PROGRESS"
}

echo "$(date '+%F %H:%M') the peak, $PEAK_MINUTES minutes as fast as answered"
soak_names "$NAME-peak"
env_check "$need" "$ENV_VOLUME" || env_failed "$RUN-s$sync-peak" "$sync:peak:env" "$ENV_WHY"
peaks="${TMPDIR:-/tmp}/$NAME.peak"
# shellcheck disable=SC2086
guarded $awake "$YCSB" soak $common --peak --minutes "$PEAK_MINUTES" --name "$NAME-peak" \
    --load-before "$(env_load)" --run-id "$RUN-s$sync-peak" >"$peaks" || true
[ -s "$ENV_MARKER" ] &&
    env_failed "$RUN-s$sync-peak" "$sync:peak:env" "$(cat "$ENV_MARKER")"
[ -s "$ENV_MARKER.hung" ] && hung "$RUN-s$sync-peak" "$sync:peak:hung" "$(cat "$ENV_MARKER.hung")"
peak=$(awk '$1 == "peak" {print $2}' "$peaks")
rm -f "$peaks"
[ -n "$peak" ] || {
    echo "no peak measured" >&2
    exit 1
}
echo "$(date '+%F %H:%M') the peak is $peak operations a second"

status=0
for f in $FRACTIONS; do
    run="$RUN-s$sync-$f"
    if [ "$status" != 0 ] && [ "$KEEP_GOING" != 1 ]; then
        echo "$(date '+%F %H:%M') $run not run: the fraction before failed (KEEP_GOING=1 runs it all the same)"
        break
    fi
    rate=$((peak * f / 100))
    echo "$(date '+%F %H:%M') $run: $MINUTES minutes at $rate a second, $f% of the peak"
    soak_names "$NAME-$f"
    env_check "$need" "$ENV_VOLUME" || env_failed "$run" "$sync:$f:env" "$ENV_WHY"
    # shellcheck disable=SC2086
    guarded $awake "$YCSB" soak $common --rate "$rate" --label "$f" --name "$NAME-$f" \
        --minutes "$MINUTES" --kill-every "$KILL_EVERY" --compact-every "$COMPACT_EVERY" \
        --backup-every "$BACKUP_EVERY" --backup-at "$BACKUP_AT" \
        --kill-in-compact-every "$KILL_IN_COMPACT_EVERY" --load-before "$(env_load)" \
        --run-id "$run" || status=1
    [ -s "$ENV_MARKER" ] && env_failed "$run" "$sync:$f:env" "$(cat "$ENV_MARKER")"
    [ -s "$ENV_MARKER.hung" ] && hung "$run" "$sync:$f:hung" "$(cat "$ENV_MARKER.hung")"
    "$YCSB" soak report "$OUT" "$run" || status=1
done
echo "$(date '+%F %H:%M') done"
exit "$status"
