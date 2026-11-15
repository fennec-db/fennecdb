# shellcheck shell=sh
# The environment's checks run-larger-than-memory.sh and run-soak.sh share,
# sourced by each from the repository's root: the machine quiet and the
# disks with room, before every step that measures and while it runs. A
# step the environment cannot take is the environment's failure, marked so
# in the results (`env_failed`), never a pass and never the server's.
#
# The script sets, before it sources this:
#   QUIET_LOAD   the one-minute load average a step may start under
#   ENV_PREFIX   p51 or p6: the checks' own container is <ENV_PREFIX>-df,
#                which the script's cleanup removes with the rest
# and may set:
#   ENV_LOAD_WAIT=900  how long a step waits for the load to fall to
#                      QUIET_LOAD -- the run's own last step lifts it, by
#                      16 threads a cell: the one-minute average falls from
#                      12 to 4 in about 70 s -- before the environment
#                      fails it
#   ENV_EVERY=30       seconds between two looks at the disks while a step
#                      runs: a container's `df` each, which costs the VM
#                      next to nothing against the server's cgroup look a
#                      second
#
# The host's free disk is `df` here; Docker's VM's is a container's `df` on
# the run's data volume (alpine, which must be local: nothing is pulled),
# on the VM's own disk, which fills on its own: its disk image has a size
# of its own, and a volume removed gives its space back to the Mac only
# when Docker Desktop trims the image.

ENV_LOAD_WAIT=${ENV_LOAD_WAIT:-900}
ENV_EVERY=${ENV_EVERY:-30}
# Why the last check failed, for the log and the marker.
ENV_WHY=
# The guard beside a step (`guarded`), which the cleanup stops.
env_guard_pid=

env_load() {
    if [ "$(uname)" = Darwin ]; then
        sysctl -n vm.loadavg | awk '{print $2}'
    else
        cut -d' ' -f1 /proc/loadavg
    fi
}

# KB free on the host's disk the repository is on.
env_host_free() {
    df -kP . | awk 'NR == 2 {print $4}'
}

# KB free on Docker's VM's disk, as a container sees it on VOLUME -- or on
# its own root, on the same disk, where the volume is not made yet: a
# mount would make it.
env_vm_free() {
    if [ -n "${1:-}" ] && docker volume inspect "$1" >/dev/null 2>&1; then
        docker run --rm --name "$ENV_PREFIX-df" -v "$1":/v:ro alpine df -kP /v
    else
        docker run --rm --name "$ENV_PREFIX-df" alpine df -kP /
    fi 2>/dev/null | awk 'NR == 2 {print $4}'
}

# env_disk NEED_KB [VOLUME]: whether the host and Docker's VM each have
# NEED_KB free; ENV_WHY names the one that has not. A disk that cannot be
# read has no room.
env_disk() {
    h=$(env_host_free)
    if [ -z "$h" ] || [ "$h" -lt "$1" ]; then
        ENV_WHY="the host's disk has ${h:-an unread number of} KB free, under the $1 KB the step needs"
        return 1
    fi
    v=$(env_vm_free "${2:-}")
    if [ -z "$v" ] || [ "$v" -lt "$1" ]; then
        ENV_WHY="Docker's VM's disk has ${v:-an unread number of} KB free, under the $1 KB the step needs"
        return 1
    fi
    return 0
}

# env_quiet [PID]: waits for the one-minute load to fall to QUIET_LOAD,
# ENV_LOAD_WAIT seconds at the most and while PID (the step asking) lives.
# QUIET_LOAD is the start's own line, never raised for a step.
env_quiet() {
    w=0
    while [ "$(env_load | awk -v q="$QUIET_LOAD" '{print ($1 > q)}')" = 1 ]; do
        if [ "$w" -ge "$ENV_LOAD_WAIT" ]; then
            ENV_WHY="the load average stayed over $QUIET_LOAD for $ENV_LOAD_WAIT s, at $(env_load)"
            return 1
        fi
        if [ -n "${1:-}" ] && ! kill -0 "$1" 2>/dev/null; then
            ENV_WHY="the step ended"
            return 1
        fi
        sleep 10
        w=$((w + 10))
    done
    return 0
}

# env_check NEED_KB [VOLUME]: before a step -- a fraction, a system's turn,
# the heap's -- the machine quiet and both disks with NEED_KB.
env_check() {
    env_quiet && env_disk "$1" "${2:-}"
}

# env_guard PID: beside a running step, until PID ends. Answers the
# questions `ycsb --gate` asks in ENV_GATE before each load and each cell
# (`ask.<id>` holding `load` or `cell`; `ok.<id>`, or `stop.<id>` holding
# why) once the load is down to QUIET_LOAD and both disks have
# ENV_NEED_LOAD or ENV_NEED_CELL KB; and every ENV_EVERY seconds holds both
# disks to ENV_FLOOR KB, stopping the step when one is under it: PID, its
# children (`caffeinate`'s) and the containers ENV_KILL names. Either way
# the reason goes to ENV_MARKER, which the script reads after the step.
# ENV_VOLUME is the volume the VM's disk is read on.
env_guard() {
    # A background shell of a `set -e` script's: a `docker rm` that finds
    # nothing must not end the guard.
    set +e
    pid=$1
    ticks=0
    while kill -0 "$pid" 2>/dev/null; do
        if [ -n "${ENV_GATE:-}" ]; then
            for ask in "$ENV_GATE"/ask.*; do
                [ -f "$ask" ] || continue
                id=${ask##*/ask.}
                if [ -f "$ENV_GATE/ok.$id" ] || [ -f "$ENV_GATE/stop.$id" ]; then
                    continue
                fi
                need=$ENV_NEED_CELL
                [ "$(cat "$ask")" = load ] && need=$ENV_NEED_LOAD
                if env_quiet "$pid" && env_disk "$need" "${ENV_VOLUME:-}"; then
                    : >"$ENV_GATE/ok.$id"
                else
                    echo "$ENV_WHY" >"$ENV_MARKER"
                    echo "$ENV_WHY" >"$ENV_GATE/stop.$id"
                fi
            done
        fi
        ticks=$((ticks + 1))
        if [ $((ticks * 2 % ENV_EVERY)) -lt 2 ] && ! env_disk "$ENV_FLOOR" "${ENV_VOLUME:-}"; then
            echo "$ENV_WHY, under the floor a running step is held to" >"$ENV_MARKER"
            pkill -TERM -P "$pid" 2>/dev/null
            kill "$pid" 2>/dev/null
            for c in ${ENV_KILL:-}; do
                docker rm -f "$c" >/dev/null 2>&1
            done
            return 0
        fi
        sleep 2
    done
    return 0
}

# guarded CMD...: CMD run as the script's `step` runs it -- in the
# background, waited for, its pid in `child` for the trap -- with
# env_guard beside it; CMD's exit status.
guarded() {
    "$@" &
    child=$!
    env_guard "$child" &
    env_guard_pid=$!
    s=0
    wait "$child" || s=$?
    child=
    env_stop_guard
    return "$s"
}

# The guard stopped and waited for: the cleanup's, and each step's end.
env_stop_guard() {
    if [ -n "$env_guard_pid" ]; then
        kill "$env_guard_pid" 2>/dev/null || true
        wait "$env_guard_pid" 2>/dev/null || true
        env_guard_pid=
    fi
    docker rm -f "$ENV_PREFIX-df" >/dev/null 2>&1 || true
    return 0
}
