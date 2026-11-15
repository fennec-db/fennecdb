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
#   nohup crates/fennec-bench/ycsb/run-larger-than-memory.sh > larger.log 2>&1 &
#
# from the repository's root, on a machine doing nothing else. Defaults:
# 4 000 000 records of 1 KB (4.4 GB live) under --memory 2g, three runs,
# cells of 30 s after a 5 s warm-up at 1 and 16 threads, three idle
# minutes between the two systems' turns: about four hours on the M1 Air.
# Each setting is an environment variable:
#
#   RECORDS=4000000 MEMORY=2g RUNS=3 CELL_SECONDS=30 WARMUP=5 THREADS=1,16
#   GAP=180 DIM=32 RUN=lm QUIET_LOAD=4 PROBE_BASE=<ms>
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
# Refused before anything starts: a live size under twice the limit, a
# Docker VM with less memory than the limit and a gigabyte, less free disk
# than the run needs (printed), a load average over QUIET_LOAD (printed).
# Everything it starts is named p51-* and removed when it ends, however it
# ends: `kill <its pid>` stops it cleanly, the lines written so far kept.
set -eu
cd "$(dirname "$0")/../../.."

RECORDS=${RECORDS:-4000000}
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

# A size as Docker writes it -- 512m, 2g -- in bytes.
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
# A record is ten fields of 100 characters and its key: 1.1 KB in the file.
live=$((RECORDS * 1100))
if [ "$live" -lt $((limit * 2)) ]; then
    echo "$RECORDS records are $((live >> 20)) MB live, under twice --memory $MEMORY: raise RECORDS" >&2
    exit 2
fi
vm=$(docker info --format '{{.MemTotal}}')
if [ "$vm" -lt $((limit + (1 << 30))) ]; then
    echo "Docker's VM has $((vm >> 20)) MB: --memory $MEMORY and a GB for the rest do not fit" >&2
    exit 2
fi
# One system's data at a time: its file, the image a compact writes beside
# it, the heap's collection, the tools' build, the image and room to spare.
need=$(( (live * 5 / 2 + RECORDS * (100 + DIM * 4)) / 1024 + (4 << 20) ))
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
cleanup() {
    [ -n "$child" ] && kill "$child" 2>/dev/null
    for c in p51-server-watch p51-server p51-engine p51-build; do
        docker rm -f "$c" >/dev/null 2>&1
    done
    for v in p51-data p51-engine-data p51-tools; do
        docker volume rm -f "$v" >/dev/null 2>&1
    done
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
cargo=cargo
[ -x "$HOME/.cargo/bin/cargo" ] && cargo="$HOME/.cargo/bin/cargo"
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

echo "$(date '+%F %H:%M') the images and tools"
step docker build -q -t fennecdb-ycsb .
step crates/fennec-bench/ycsb/linux-tools.sh p51
step "$cargo" build --release -q -p fennec-bench --bin ycsb --no-default-features

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

r=1
while [ "$r" -le "$RUNS" ]; do
    id="$RUN$r"
    echo "$(date '+%F %H:%M') run $r: fennec-server in Docker under --memory $MEMORY"
    # shellcheck disable=SC2086
    if ! step $awake ./target/release/ycsb --systems server-docker --docker-name p51 \
        $args --run-id "$id" --out "$OUT" --verify "$VERIFY" $probe; then
        # shellcheck disable=SC2086
        ./target/release/ycsb fill --systems server-docker $args --run-id "$id" \
            --out "$OUT" --oom "$(oom p51-server)"
    fi
    docker rm -f p51-server-watch p51-server >/dev/null 2>&1 || true
    docker volume rm -f p51-data >/dev/null 2>&1 || true
    echo "$(date '+%F %H:%M') idle $GAP s"
    sleep "$GAP"

    echo "$(date '+%F %H:%M') run $r: fennecdb in process in a container under --memory $MEMORY"
    # shellcheck disable=SC2086
    step $awake docker run --name p51-engine --memory "$MEMORY" --memory-swap "$MEMORY" \
        -e TMPDIR=/work -e FENNEC_BENCH_COMMIT -e FENNEC_BENCH_MACHINE \
        -v p51-tools:/tools:ro -v p51-engine-data:/work \
        -v "$outdir":/out -v "$verifydir":/verify \
        alpine /tools/ycsb --systems fennec $args --run-id "$id" \
        --out "/out/$(basename "$OUT")" --verify "/verify/$(basename "$VERIFY")" \
        --verify-max "$vmax" $probe || true
    code=$(docker inspect -f '{{.State.ExitCode}}' p51-engine 2>/dev/null || echo 1)
    if [ "$code" != 0 ]; then
        echo "the process in the container ended with $code"
        # shellcheck disable=SC2086
        ./target/release/ycsb fill --systems fennec $args --run-id "$id" \
            --out "$OUT" --oom "$(oom p51-engine)"
    fi
    docker rm -f p51-engine >/dev/null 2>&1 || true
    docker volume rm -f p51-engine-data >/dev/null 2>&1 || true
    if [ "$r" -lt "$RUNS" ]; then
        echo "$(date '+%F %H:%M') idle $GAP s"
        sleep "$GAP"
    fi
    r=$((r + 1))
done

echo "$(date '+%F %H:%M') the heap a row of each index kind, $RECORDS rows"
# shellcheck disable=SC2086
step $awake ./target/release/ycsb heap --records "$RECORDS" --memory "$MEMORY" \
    --docker-name p51 --family ycsb_larger --run-id "${RUN}h" --out "$OUT" --dim "$DIM" ||
    echo "the heap's measurement ended early: its lines say which kinds"
echo "$(date '+%F %H:%M') done"
./target/release/ycsb report "$OUT"
