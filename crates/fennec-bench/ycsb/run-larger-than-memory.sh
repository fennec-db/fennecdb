#!/bin/sh
# YCSB's B, C and E over live data at least twice the memory it may use:
# fennec-server in a container whose --memory is the limit
# (`server-docker`), then fennecdb in process in a container under the
# same limit, durable and buffered, every answer held to what was written
# (--verify), each cell with its cgroup's major faults a second and its
# memory beside it; then the heap a row of each index kind holds at the
# same size, from the server's own memory_bytes (`ycsb heap`). The file is
# mapped and there is no buffer pool: what does not fit is read again from
# the disk, which is what this measures.
#
#   crates/fennec-bench/ycsb/run-larger-than-memory.sh --preflight
#   nohup crates/fennec-bench/ycsb/run-larger-than-memory.sh > larger.log 2>&1 &
#   crates/fennec-bench/ycsb/run-larger-than-memory.sh --self-test
#
# from the repository's root, on a machine doing nothing else. Defaults:
# 4 700 000 records of 1 KB (4.8 GB live) under --memory 2g, three runs,
# cells of 30 s after a 5 s warm-up at 1 and 16 threads, three idle
# minutes between the two systems' turns: about four hours on the M1 Air,
# more where a cell waits for the load its last one left to fall. Each
# setting is an environment variable:
#
#   RECORDS=4700000 MEMORY=2g RUNS=3 CELL_SECONDS=30 WARMUP=5 THREADS=1,16
#   GAP=180 DIM=32 RUN=lm QUIET_LOAD=4 PROBE_BASE=<ms>
#   ENV_LOAD_WAIT=900 ENV_EVERY=30 (ycsb/env-check.sh)
#   OUT=crates/fennec-bench/results/ycsb_larger.tsv
#   VERIFY=crates/fennec-bench/ycsb/verify-larger.tsv
#
# The lines go to OUT, family `ycsb_larger`: a file of its own, not
# ycsb.tsv, because its cells are named as the published grid's
# (`server-docker:durable:B:16`) and site/build.py and `ycsb report` take
# the median of every run of a cell -- a memory-bound run there would join
# the medians of the runs with the page cache to themselves. Every cell
# asked for has a line: one that failed, whose system was killed for its
# memory or whose process ended says so (`failed`, `oom`; `ycsb fill`
# writes the ones a killed process could not). `./target/release/ycsb
# report $OUT` prints the medians, the faults, the memory and the failures.
#
# Refused before anything starts: a live size under twice the limit at the
# smallest record measured, a Docker VM with less memory than the limit
# and a gigabyte, less free disk than the run needs (printed), a load
# average over QUIET_LOAD (printed). After each load, before any cell,
# `ycsb` reads the live bytes and the cgroup's own limit (memory.max) and
# refuses, naming both, live bytes under twice it: the run ends there.
# Before each system's turn and, through `ycsb --gate`, before each load
# and each cell the load must be down to QUIET_LOAD (waited for,
# ENV_LOAD_WAIT s at most) and the host's and Docker's VM's disks must have
# room, and while a step runs both disks are held to a floor
# (ycsb/env-check.sh): a step the environment cannot take ends the run, its
# cells written `env_failed` -- the environment's failure, never a pass
# and never the server's. `--preflight` says what must be on this machine
# first, which a run checks before it starts anything (ycsb/env-check.sh):
# nothing is pulled while it runs. Everything it starts is named p51-<its
# pid>-*, so that a run never removes another's, and removed when it ends,
# however it ends: `kill <its pid>` stops it cleanly, the measurement under
# `caffeinate` too (`--self-test` holds the trap to it with toys), the
# lines written so far kept.
set -eu
cd "$(dirname "$0")/../../.."
ENV_PREFIX=p51
. crates/fennec-bench/ycsb/env-check.sh
# Until the run's own cleanup takes over: what a bounded docker call was
# in, stopped, however the script ends (the pre-flight's build among them).
trap env_end EXIT
trap 'exit 130' INT TERM
case ${1:-} in
--self-test)
    env_self_test crates/fennec-bench/ycsb/run-larger-than-memory.sh ''
    exit
    ;;
--preflight)
    env_preflight
    exit
    ;;
esac

# Twice the limit at the least: the dry run's records were 1 026 to 1 059
# bytes live each (550 000 of them, 566-582 MB), so 4 000 000 under 2g held
# 1.92 to 1.96 times it. 4 700 000 x 1 026 = 4.82 GB, 2.24 times 2 GiB at
# the smallest record measured (2.32 at the largest).
RECORDS=${RECORDS:-4700000}
MEMORY=${MEMORY:-2g}
RUNS=${RUNS:-3}
CELL_SECONDS=${CELL_SECONDS:-30}
WARMUP=${WARMUP:-5}
THREADS=${THREADS:-1,16}
GAP=${GAP:-180}
DIM=${DIM:-32}
RUN=${RUN:-lm}
QUIET_LOAD=${QUIET_LOAD:-4}
OUT=${OUT:-crates/fennec-bench/results/ycsb_larger.tsv}
VERIFY=${VERIFY:-crates/fennec-bench/ycsb/verify-larger.tsv}
# What the run is made of, each replaced by a toy in `--self-test`.
YCSB=${YCSB:-./target/release/ycsb}
LINUX_TOOLS=${LINUX_TOOLS:-crates/fennec-bench/ycsb/linux-tools.sh}
# The run's own names, its process id in each.
P=p51-$$
ENV_PREFIX=$P

# A size as Docker writes it -- 512m, 2g -- in bytes.
bytes() {
    echo "$1" | awk '{
        n = $0 + 0; u = tolower(substr($0, length($0)))
        if (u == "k") n *= 1024; else if (u == "m") n *= 1048576; else if (u == "g") n *= 1073741824
        printf "%.0f\n", n }'
}

limit=$(bytes "$MEMORY")
# A record is ten fields of 100 characters and its key: 1 026 to 1 059
# bytes live as the dry run measured them. Held to twice the limit at the
# smallest; the disk counted at 1.1 KB, above the largest.
least=$((RECORDS * 1026))
if [ "$least" -lt $((limit * 2)) ]; then
    echo "$RECORDS records are $((least >> 20)) MB live at 1 026 bytes each, under twice --memory $MEMORY: raise RECORDS" >&2
    exit 2
fi
live=$((RECORDS * 1100))
vm=$(env_within 60 "docker info" docker info --format '{{.MemTotal}}')
if [ "$vm" -lt $((limit + (1 << 30))) ]; then
    echo "Docker's VM has $((vm >> 20)) MB: --memory $MEMORY and a GB for the rest do not fit" >&2
    exit 2
fi
# One system's data at a time: its file, the image a compact writes beside
# it, the heap's collection, the tools' build, the image and room to spare.
need=$(( (live * 5 / 2 + RECORDS * (100 + DIM * 4)) / 1024 + (4 << 20) ))
# What each step needs of both disks, in KB, checked before it: a system's
# load, its file and a compact's image beside it; a cell, a compact's image
# of the live records and a GB; the heap's collection twice and a GB. A
# running step is stopped under a GB on either (ENV_FLOOR), before a write
# fails for want of it.
turn=$(( live * 5 / 2 / 1024 + (1 << 20) ))
ENV_NEED_LOAD=$turn
ENV_NEED_CELL=$(( live / 1024 + (1 << 20) ))
ENV_FLOOR=$((1 << 20))
heap_need=$(( RECORDS * (100 + DIM * 4) * 2 / 1024 + (1 << 20) ))
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
# The questions `ycsb --gate` asks and the guard's verdict (ENV_MARKER):
# a directory a container's run mounts too.
gate=$(mktemp -d "${TMPDIR:-/tmp}/p51-gate.XXXXXX")
ENV_GATE=$gate
ENV_MARKER=$gate/env-failed
# shellcheck disable=SC2329 # the EXIT trap's
cleanup() {
    env_stop_child
    env_stop_guard
    env_within 120 "docker rm" docker rm -f "$P-server-watch" "$P-server-look" "$P-server" \
        "$P-engine" "$P-build" "$P-df" >/dev/null 2>&1
    env_within 120 "docker volume rm" docker volume rm -f "$P-data" "$P-engine-data" "$P-tools" >/dev/null 2>&1
    [ -n "$gate" ] && [ -d "$gate" ] && rm -rf "$gate"
    env_end
    return 0
}
trap cleanup EXIT
trap 'exit 130' INT TERM
# A step run in the background and waited for, so a signal reaches the
# trap at once and the trap stops it.
step() {
    "$@" &
    child=$!
    s=0
    wait "$child" || s=$?
    child=
    return "$s"
}

# rustup's cargo, never another on the PATH (CLAUDE.md).
cargo=${CARGO:-cargo}
[ -z "${CARGO:-}" ] && [ -x "$HOME/.cargo/bin/cargo" ] && cargo="$HOME/.cargo/bin/cargo"
awake=""
if command -v caffeinate >/dev/null 2>&1; then
    awake="caffeinate -i"
fi

commit=$(git rev-parse --short=8 HEAD)
[ -z "$(git status --porcelain --untracked-files=no)" ] || commit="$commit-dirty"
machine=${FENNEC_BENCH_MACHINE:-$(hostname -s)}
export FENNEC_BENCH_COMMIT="$commit" FENNEC_BENCH_MACHINE="$machine"
mkdir -p "$(dirname "$OUT")" "$(dirname "$VERIFY")"
outdir=$(cd "$(dirname "$OUT")" && pwd)
verifydir=$(cd "$(dirname "$VERIFY")" && pwd)

env_preflight || {
    echo "the pre-flight failed: nothing was started" >&2
    exit 2
}
echo "$(date '+%F %H:%M') the tools"
step "$LINUX_TOOLS" "$P"
step "$cargo" build --release --offline -q -p fennec-bench --bin ycsb --no-default-features

probe=${PROBE_BASE:+--probe-base $PROBE_BASE}
args="--records $RECORDS --memory $MEMORY --family ycsb_larger --workloads BCE
    --modes durable,buffered --threads $THREADS --seconds $CELL_SECONDS
    --warmup $WARMUP --runs 1 --turn-gap 0"
# The verify logs of a cell in process are its process's memory, under the
# limit: half of it, then the cell's window ends (`--verify-max`).
vmax=$((limit / 2 >> 20))
oom() {
    [ "$(docker inspect -f '{{.State.OOMKilled}}' "$1" 2>/dev/null)" = true ] && echo 1 || echo 0
}
# The environment failed a system's turn of run $2 ($1: server-docker or
# fennec): its cells not measured, and those failed -- a server meeting a
# full disk fails its cell -- written as the environment's, and the run
# ends, the cleanup removing what it started.
env_failed() {
    echo "$(date '+%F %H:%M') the environment's failure: $3" >&2
    # shellcheck disable=SC2086
    "$YCSB" fill --systems "$1" $args --run-id "$2" --out "$OUT" --env-failed 1
    "$YCSB" report "$OUT" || true
    exit 3
}
# ycsb refused the run after a load: the live bytes under twice the
# cgroup's limit, both named. No cell ran; the run ends.
refused() {
    echo "$(date '+%F %H:%M') refused: $(cat "$gate/refused")" >&2
    exit 2
}

r=1
while [ "$r" -le "$RUNS" ]; do
    id="$RUN$r"
    echo "$(date '+%F %H:%M') run $r: fennec-server in Docker under --memory $MEMORY"
    env_check "$turn" "$P-data" || env_failed server-docker "$id" "$ENV_WHY"
    ENV_VOLUME="$P-data" ENV_KILL="$P-server $P-server-watch $P-server-look"
    s=0
    # shellcheck disable=SC2086
    guarded $awake "$YCSB" --systems server-docker --docker-name "$P" \
        $args --run-id "$id" --out "$OUT" --verify "$VERIFY" --gate "$gate" $probe || s=$?
    [ -s "$gate/refused" ] && refused
    [ -s "$ENV_MARKER" ] && env_failed server-docker "$id" "$(cat "$ENV_MARKER")"
    if [ "$s" != 0 ]; then
        # shellcheck disable=SC2086
        "$YCSB" fill --systems server-docker $args --run-id "$id" \
            --out "$OUT" --oom "$(oom "$P-server")"
    fi
    docker rm -f "$P-server-watch" "$P-server" >/dev/null 2>&1 || true
    docker volume rm -f "$P-data" >/dev/null 2>&1 || true
    echo "$(date '+%F %H:%M') idle $GAP s"
    sleep "$GAP"

    echo "$(date '+%F %H:%M') run $r: fennecdb in process in a container under --memory $MEMORY"
    env_check "$turn" "$P-engine-data" || env_failed fennec "$id" "$ENV_WHY"
    ENV_VOLUME="$P-engine-data" ENV_KILL="$P-engine"
    # shellcheck disable=SC2086
    guarded $awake docker run --pull never --name "$P-engine" --memory "$MEMORY" --memory-swap "$MEMORY" \
        -e TMPDIR=/work -e FENNEC_BENCH_COMMIT -e FENNEC_BENCH_MACHINE \
        -v "$P-tools":/tools:ro -v "$P-engine-data":/work \
        -v "$outdir":/out -v "$verifydir":/verify -v "$gate":/gate \
        "$ENV_ALPINE" /tools/ycsb --systems fennec $args --run-id "$id" \
        --out "/out/$(basename "$OUT")" --verify "/verify/$(basename "$VERIFY")" \
        --verify-max "$vmax" --gate /gate $probe || true
    [ -s "$gate/refused" ] && refused
    [ -s "$ENV_MARKER" ] && env_failed fennec "$id" "$(cat "$ENV_MARKER")"
    code=$(docker inspect -f '{{.State.ExitCode}}' "$P-engine" 2>/dev/null || echo 1)
    if [ "$code" != 0 ]; then
        echo "the process in the container ended with $code"
        # shellcheck disable=SC2086
        "$YCSB" fill --systems fennec $args --run-id "$id" \
            --out "$OUT" --oom "$(oom "$P-engine")"
    fi
    docker rm -f "$P-engine" >/dev/null 2>&1 || true
    docker volume rm -f "$P-engine-data" >/dev/null 2>&1 || true
    if [ "$r" -lt "$RUNS" ]; then
        echo "$(date '+%F %H:%M') idle $GAP s"
        sleep "$GAP"
    fi
    r=$((r + 1))
done

echo "$(date '+%F %H:%M') the heap a row of each index kind, $RECORDS rows"
# With no --memory: @hnsw over millions of vectors is heap that has to fit,
# and under the cells' limit it is killed, which says nothing of what it
# takes. The VM's memory is the bound; the lines say the engine's count
# (bytes_row) and what the cgroup's anonymous memory grew by
# (cg_anon_bytes_row).
heap_failed() {
    echo "$(date '+%F %H:%M') the environment's failure: $1" >&2
    for kind in hash sorted text hnsw; do
        # A kind measured before the disk ran short stays as measured.
        awk -F'\t' -v r="${RUN}h" -v c="server-docker:heap:$kind" \
            '$1 == r && $6 == c && $7 == "bytes_row" {found = 1} END {exit !found}' "$OUT" ||
            "$YCSB" mark "$OUT" ycsb_larger "${RUN}h" "server-docker:heap:$kind" env_failed 1
    done
    exit 3
}
env_check "$heap_need" "$P-data" || heap_failed "$ENV_WHY"
ENV_GATE='' ENV_VOLUME="$P-data" ENV_KILL="$P-server $P-server-look"
# shellcheck disable=SC2086
guarded $awake "$YCSB" heap --records "$RECORDS" \
    --docker-name "$P" --family ycsb_larger --run-id "${RUN}h" --out "$OUT" --dim "$DIM" ||
    echo "the heap's measurement ended early: its lines say which kinds"
[ -s "$ENV_MARKER" ] && heap_failed "$(cat "$ENV_MARKER")"
echo "$(date '+%F %H:%M') done"
"$YCSB" report "$OUT"
