# shellcheck shell=sh
# The environment's checks run-larger-than-memory.sh and run-soak.sh share,
# sourced by each from the repository's root: the machine quiet and the
# disks with room, before every step that measures and while it runs. A
# step the environment cannot take is the environment's failure, marked so
# in the results (`env_failed`), never a pass and never the server's.
#
# The script sets, before it calls these:
#   QUIET_LOAD   the one-minute load average a step may start under
#   ENV_PREFIX   the run's own name (p51-<pid>, p6-...-<pid>): the checks'
#                own container is <ENV_PREFIX>-df, which the script's cleanup
#                removes with the rest
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
#   ENV_PROGRESS       a file the step writes a line to as it goes (`ycsb
#                      soak --progress`): a step whose file gained nothing
#                      in ENV_SILENT seconds (600) is stopped as hung, its
#                      reason in ENV_MARKER.hung
#
# The host's free disk is `df` here; Docker's VM's is a container's `df` on
# the run's data volume (alpine by its digest, which must be local: nothing
# is pulled),
# on the VM's own disk, which fills on its own: its disk image has a size
# of its own, and a volume removed gives its space back to the Mac only
# when Docker Desktop trims the image.
#
# Every docker call the guard makes is bounded (`env_within`): a daemon
# that stopped answering would hang the guard, and with it the check that
# a step still prints. `env_preflight` says what a run needs before its
# first command; `env_self_test` holds a script's trap and guard to their
# promises with toys.
#
# A script that sources this ends with `env_end` on every exit (its trap):
# what a bounded call it was in started is stopped, and ENV_DIR removed.

ENV_LOAD_WAIT=${ENV_LOAD_WAIT:-900}
ENV_EVERY=${ENV_EVERY:-30}
ENV_SILENT=${ENV_SILENT:-600}
# Why the last check failed, for the log and the marker.
ENV_WHY=
# The guard beside a step (`guarded`), which the cleanup stops.
env_guard_pid=
# The image the looks, verifies, backups, sampler and `df` run in, by its
# digest as the Dockerfile names the rust image: nothing is pulled, and a
# tag can move. ycsb names the same (`ALPINE` in src/bin/ycsb.rs, a test
# holding the two to one digest); `--preflight` checks it is here.
ENV_ALPINE=alpine@sha256:f59abe656b692d901e184be3445e084786602108599d2a51d229c0f5a48c71bf
# The servers' image: built by `--preflight` alone, labelled with the
# identity of the tree it was built from (`env_files`), and run by its id
# once a run found that label to be its own tree's (`env_image`).
ENV_IMAGE_TAG=fennecdb-ycsb
ENV_IMAGE=
# The shell's own directory: what each bounded call started (`env_within`)
# and a `df`'s answer (ENV_DF, the guard's apart).
ENV_DIR=$(mktemp -d "${TMPDIR:-/tmp}/ycsb-env.XXXXXX")
ENV_DF=$ENV_DIR/df

# env_within SECS WHAT CMD...: CMD, stopped after SECS with WHAT named on
# stderr; CMD's status (143 when it was stopped). No `timeout` on a Mac.
#
# What it starts -- CMD and the watch beside it -- is written to ENV_DIR
# until it has waited for both, so that the cleanup and env_stop_guard stop
# them by pid wherever the call was made: in the script's shell, in the
# guard's, or in a subshell of either, whose variables they never see. The
# watch looks once a second rather than sleeping SECS in one go: a watch
# stopped leaves at most a second's `sleep`, which ends on its own, where
# one stopped in its SECS left that sleep running for up to half an hour.
env_within() {
    ew_secs=$1 ew_what=$2
    shift 2
    "$@" &
    ew_pid=$!
    (
        # The shell's word on its sleep stopped is no one's business; and
        # no stdout of the caller's: a `$(env_within ...)` reads until
        # every holder of its pipe is gone, and a watch's sleep left
        # behind held it -- for 120 s, when the watch slept SECS whole.
        exec 3>&2 2>/dev/null >/dev/null
        ew_n=0
        while kill -0 "$ew_pid"; do
            if [ "$ew_n" -ge "$ew_secs" ]; then
                if kill -TERM "$ew_pid"; then
                    echo "$ew_what: no end within $ew_secs s, stopped" >&3
                fi
                exit 0
            fi
            sleep 1
            ew_n=$((ew_n + 1))
        done
    ) &
    ew_watch=$!
    echo "$ew_pid $ew_watch" 2>/dev/null >"$ENV_DIR/within.$ew_pid" || true
    ew_s=0
    wait "$ew_pid" || ew_s=$?
    # The watch's status is its stop's, never the command's (and never
    # `set -e`'s to act on); its second's sleep first, where it can be.
    pkill -P "$ew_watch" 2>/dev/null || true
    kill "$ew_watch" 2>/dev/null || true
    wait "$ew_watch" 2>/dev/null || true
    rm -f "$ENV_DIR/within.$ew_pid"
    return "$ew_s"
}

# What env_within started and has not waited for, wherever it was called,
# stopped by pid: the cleanup's first, before calls of its own, and
# env_stop_guard's, after the guard.
env_stop_within() {
    for ew_file in "$ENV_DIR"/within.*; do
        [ -f "$ew_file" ] || continue
        # shellcheck disable=SC2046 # the pids, a word each
        kill -TERM $(cat "$ew_file") 2>/dev/null || true
        rm -f "$ew_file"
    done
    return 0
}

# The end of a shell that sourced this: what a bounded call left, stopped,
# and ENV_DIR removed. Every trap's last word.
env_end() {
    env_stop_within
    case $ENV_DIR in
    */ycsb-env.*) rm -rf "$ENV_DIR" ;;
    esac
    return 0
}

# The step running in the background (`child`), stopped: its children
# first -- `caffeinate -i` runs the measurement as its child and passes no
# signal on, so a TERM to it alone left ycsb running, writing lines and,
# where it panicked later, removing containers by name -- then itself.
env_stop_child() {
    if [ -n "${child:-}" ]; then
        pkill -TERM -P "$child" 2>/dev/null || true
        kill -TERM "$child" 2>/dev/null || true
        wait "$child" 2>/dev/null || true
        child=
    fi
    return 0
}

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

# KB free on Docker's VM's disk in ENV_VM_FREE, as a container sees it on
# VOLUME -- or on its own root, on the same disk, where the volume is not
# made yet: a mount would make it. Each call in the caller's own shell,
# none in a pipeline's or a command substitution's: a stop of the guard
# reaches what its own shell started, where a subshell of it, stopped
# around, went on to its next call.
env_vm_free() {
    ENV_VM_FREE=
    if [ -n "${1:-}" ] && env_within 60 "docker volume inspect" docker volume inspect "$1" >/dev/null 2>&1; then
        env_within 120 "Docker's df on $1" \
            docker run --rm --pull never --name "$ENV_PREFIX-df" -v "$1":/v:ro "$ENV_ALPINE" df -kP /v \
            >"$ENV_DF" 2>/dev/null || true
    else
        env_within 120 "Docker's df" \
            docker run --rm --pull never --name "$ENV_PREFIX-df" "$ENV_ALPINE" df -kP / \
            >"$ENV_DF" 2>/dev/null || true
    fi
    ENV_VM_FREE=$(awk 'NR == 2 {print $4}' "$ENV_DF" 2>/dev/null) || ENV_VM_FREE=
    rm -f "$ENV_DF"
    return 0
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
    env_vm_free "${2:-}"
    v=$ENV_VM_FREE
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
# ENV_VOLUME is the volume the VM's disk is read on. Where ENV_PROGRESS is
# set, a step whose file gained nothing in ENV_SILENT seconds is stopped
# the same way as hung, its reason in ENV_MARKER.hung: a soak blocked on
# a call that never returns stops every line, and would wait forever.
env_guard() {
    # A background shell of a `set -e` script's: a `docker rm` that finds
    # nothing must not end the guard. What it finds goes to its marker
    # files; its stderr is the shell's word on a sleep it was stopped in.
    set +e
    exec 2>/dev/null
    # Its `df` answers in a file of its own beside the script shell's.
    ENV_DF=$ENV_DIR/df.guard
    pid=$1
    ticks=0
    said=-1
    said_at=$(date +%s)
    while kill -0 "$pid" 2>/dev/null; do
        if [ -n "${ENV_PROGRESS:-}" ]; then
            size=$(wc -c <"$ENV_PROGRESS" 2>/dev/null || echo 0)
            now=$(date +%s)
            if [ "$size" != "$said" ]; then
                said=$size
                said_at=$now
            elif [ $((now - said_at)) -ge "$ENV_SILENT" ]; then
                echo "the step printed nothing for $((now - said_at)) s: stopped as hung" >"$ENV_MARKER.hung"
                pkill -TERM -P "$pid" 2>/dev/null
                kill "$pid" 2>/dev/null
                if [ -n "${ENV_KILL:-}" ]; then
                    # shellcheck disable=SC2086
                    env_within 120 "docker rm" docker rm -f $ENV_KILL >/dev/null 2>&1
                fi
                return 0
            fi
        fi
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
            if [ -n "${ENV_KILL:-}" ]; then
                # shellcheck disable=SC2086
                env_within 120 "docker rm" docker rm -f $ENV_KILL >/dev/null 2>&1
            fi
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

# The guard stopped and waited for: the cleanup's, and each step's end --
# its children too, a `sleep` or a bounded docker call it was in, the
# call's command and watch by the pids it wrote (`env_stop_within`). The
# guard is held still while its children are stopped: let run, it could
# start its next call between the two.
env_stop_guard() {
    if [ -n "$env_guard_pid" ]; then
        kill -STOP "$env_guard_pid" 2>/dev/null || true
        pkill -TERM -P "$env_guard_pid" 2>/dev/null || true
        kill -TERM "$env_guard_pid" 2>/dev/null || true
        kill -CONT "$env_guard_pid" 2>/dev/null || true
        wait "$env_guard_pid" 2>/dev/null || true
        env_guard_pid=
    fi
    env_stop_within
    env_within 60 "docker rm" docker rm -f "$ENV_PREFIX-df" >/dev/null 2>&1 || true
    return 0
}

# env_files: this tree's identity, which the pre-flight's build labels the
# image with and every run holds the image to: a hash of every file git
# tracks, or sees untracked and not ignored, with its path, as its bytes are
# on disk now (`--no-filters`: what the build's COPY takes) -- but for the
# harness's own crate, crates/fennec-bench (its sources, and the results and
# verify logs its runs write, none of which the server is built from),
# .claude/ (agents' files and worktrees, which .dockerignore leaves out of
# the build) and untracked files at the root (a run's log, `> soak.log`).
# Every other file counts, whether the server reads it or not: it embeds
# studio/ and web/ files, and a list of what it reads would miss the next
# one; a change to the docs costs a pre-flight, never a stale server. A
# commit and a dirty flag would not do: a run's own lines dirty the tree,
# and a dirty tree says nothing of which changes. Nothing when git cannot
# list the tree (1). Under a second for 1 900 files.
env_files() {
    ef_list=$ENV_DIR/tree
    {
        git ls-files -z --cached -- . ':(exclude)crates/fennec-bench' ':(exclude).claude' &&
            git ls-files -z --others --exclude-standard -- '*/*' \
                ':(exclude)crates/fennec-bench' ':(exclude).claude'
    } 2>/dev/null | tr '\0' '\n' | while IFS= read -r ef_f; do
        # A tracked file deleted from the disk is not in the build either.
        if [ -f "$ef_f" ]; then
            printf '%s\n' "$ef_f"
        fi
    done >"$ef_list"
    if [ ! -s "$ef_list" ]; then
        rm -f "$ef_list"
        return 1
    fi
    ef_sum=$(git hash-object --no-filters --stdin-paths <"$ef_list" |
        paste -d ' ' - "$ef_list" | git hash-object --stdin) || ef_sum=
    rm -f "$ef_list"
    [ -n "$ef_sum" ] || return 1
    echo "$ef_sum"
}

# env_commit: the commit this tree is at, `-dirty` where a tracked file
# env_files counts differs from it: the label's words for a person, never
# what a run is held to.
env_commit() {
    ec_c=$(git rev-parse --short=12 HEAD 2>/dev/null) || ec_c=unknown
    git diff --quiet HEAD -- . ':(exclude)crates/fennec-bench' ':(exclude).claude' 2>/dev/null ||
        ec_c="$ec_c-dirty"
    echo "$ec_c"
}

# env_image TREE: whether the image tagged ENV_IMAGE_TAG is one the
# pre-flight built from a tree whose identity is TREE (its label
# com.fennecdb.tree); its id in ENV_IMAGE then, the image every container
# of the run starts from -- never the tag, which a build from another tree
# (run-redis.sh's, `make ycsb`'s, another checkout's) can move while a run
# runs. ENV_WHY says why not. The labels are read by ranging over the
# config: `.Config.Labels` is an error, not an empty map, on an image that
# has none.
env_image() {
    ENV_IMAGE=
    ei_out=$ENV_DIR/image
    env_within 60 "docker image inspect" docker image inspect --format \
        '{{.Id}}{{range $k, $v := .Config}}{{if eq $k "Labels"}}{{range $l, $w := $v}} {{$l}}={{$w}}{{end}}{{end}}{{end}}' \
        "$ENV_IMAGE_TAG" >"$ei_out" 2>/dev/null || : >"$ei_out"
    ei_id=$(awk 'NR == 1 {print $1}' "$ei_out")
    ei_tree=$(tr ' ' '\n' <"$ei_out" | sed -n 's/^com\.fennecdb\.tree=//p')
    ei_commit=$(tr ' ' '\n' <"$ei_out" | sed -n 's/^com\.fennecdb\.commit=//p')
    rm -f "$ei_out"
    case $ei_id in
    sha256:*) ;;
    *)
        ENV_WHY="no image $ENV_IMAGE_TAG on this machine"
        return 1
        ;;
    esac
    if [ -z "$ei_tree" ]; then
        ENV_WHY="$ENV_IMAGE_TAG ($(printf '%.19s' "$ei_id")) carries no tree's label: built by hand or by another script"
        return 1
    fi
    if [ "$ei_tree" != "$1" ]; then
        ENV_WHY="built from another tree (its files $(printf '%.12s' "$ei_tree") at ${ei_commit:-an unnamed commit}; this tree's $(printf '%.12s' "$1") at $(env_commit))"
        return 1
    fi
    ENV_IMAGE=$ei_id
    return 0
}

# env_preflight [build]: what a run needs before its first command, each
# looked at and said -- `ok` or `MISSING` with how to get it -- so that a
# run fails in words before anything starts, and nothing is pulled or
# fetched while it runs: every `docker run` of the scripts and of ycsb
# takes `--pull never`, the tools build with no network, and a run builds
# no image. 1 when one is missing.
#
#   the rust image the Dockerfile names, by its digest (the tag alone can
#     move), which ycsb/linux-tools.sh builds the Linux tools in;
#   alpine by its digest (ENV_ALPINE), which the looks, verifies, backups
#     and df run in: run, first (below);
#   rustup's toolchain rust-toolchain.toml names, with its targets: a
#     cargo in this tree installs a missing one from the network;
#   cargo's registry index, which the tools' build reads offline;
#   the image `fennecdb-ycsb` built from this tree: labelled with this
#     tree's identity (`env_files`, `env_image`), its id in ENV_IMAGE.
#
# With `build` -- `--preflight`, in a window where the network may be used
# -- an image not labelled with this tree is built, with the network as a
# plain `docker build` has it, and labelled; a run passes no argument and
# only looks. Not `docker build --network=none` at every run, as before:
# BuildKit keys a RUN step's cache by its network mode, so a build with no
# network never reuses an online build's apk layer and wants the network
# for it -- the check could not pass on a machine whose online build had
# just succeeded.
env_preflight() {
    pf_bad=0
    pf() {
        if [ "$1" = 0 ]; then
            echo "  ok       $2"
        else
            echo "  MISSING  $2: $3"
            pf_bad=1
        fi
    }
    echo "$(date '+%F %H:%M') pre-flight: what the run needs before it starts"
    pf_s=0
    env_within 60 "docker info" docker info --format '{{.MemTotal}}' >/dev/null 2>&1 || pf_s=1
    pf "$pf_s" "Docker answering" "start Docker Desktop"
    # alpine is run, not inspected, and first. On this Mac (Docker Desktop
    # 29.2.1, 2026-11-22) `docker image inspect` answered "No such image"
    # for every image, by tag, digest or platform, while `docker image ls`
    # listed them all -- twice, after the VM had idled some minutes -- and
    # a `docker run` answered truly and woke it, every inspect after it
    # answering truly again. So the run holds alpine to what every look
    # does with it (`--pull never`), and the inspects come after it.
    pf_s=0
    env_within 120 "docker run $ENV_ALPINE" docker run --rm --pull never --network none \
        --name "$ENV_PREFIX-df" "$ENV_ALPINE" true >/dev/null 2>&1 || pf_s=1
    pf "$pf_s" "the image $ENV_ALPINE, by its digest, runs" \
        "docker pull $ENV_ALPINE, where the network may be used"
    rust=$(awk '/^FROM rust:[^ ]*@sha256:/ {print $2; exit}' Dockerfile)
    pf_s=0
    [ -n "$rust" ] && env_within 60 "docker image inspect" docker image inspect "$rust" >/dev/null 2>&1 || pf_s=1
    pf "$pf_s" "the image ${rust:-the Dockerfile names}, by its digest" \
        "docker pull $rust, where the network may be used"
    rustup=rustup
    command -v rustup >/dev/null 2>&1 || rustup=$HOME/.cargo/bin/rustup
    channel=$(sed -n 's/^channel *= *"\([^"]*\)".*/\1/p' rust-toolchain.toml)
    pf_s=0
    "$rustup" toolchain list 2>/dev/null | grep -q "^$channel" || pf_s=1
    pf "$pf_s" "rustup's toolchain $channel" "rustup toolchain install $channel"
    for t in $(sed -n 's/^targets *= *\[\(.*\)\].*/\1/p' rust-toolchain.toml | tr -d '",'); do
        pf_s=0
        "$rustup" target list --installed --toolchain "$channel" 2>/dev/null | grep -qx "$t" || pf_s=1
        pf "$pf_s" "the target $t of $channel" "rustup target add --toolchain $channel $t"
    done
    pf_s=0
    [ -d "${CARGO_HOME:-$HOME/.cargo}/registry/index" ] || pf_s=1
    pf "$pf_s" "cargo's registry index" "build the workspace once on this machine"
    pf_what="$ENV_IMAGE_TAG built from this tree"
    pf_how="run --preflight where the network may be used, which builds it"
    pf_tree=$(env_files) || pf_tree=
    if [ -z "$pf_tree" ]; then
        pf 1 "$pf_what" "git cannot list this tree's files: run from a git checkout"
    elif env_image "$pf_tree"; then
        pf 0 "$pf_what (its files $(printf '%.12s' "$pf_tree")), run as $(printf '%.19s' "$ENV_IMAGE")"
    elif [ "${1:-}" != build ]; then
        pf 1 "$pf_what" "$ENV_WHY: $pf_how"
    elif [ "$pf_bad" != 0 ]; then
        # Not built while something above is missing: the build would
        # fetch the rust image itself.
        pf 1 "$pf_what" "$ENV_WHY: not built while a line above is MISSING"
    else
        echo "  building $ENV_IMAGE_TAG from this tree (its files $(printf '%.12s' "$pf_tree")), the network allowed: $ENV_WHY"
        pf_s=0
        env_within 1800 "the image's build" \
            docker build -q --label "com.fennecdb.tree=$pf_tree" \
            --label "com.fennecdb.commit=$(env_commit)" -t "$ENV_IMAGE_TAG" . \
            >"$ENV_DIR/build" 2>&1 || pf_s=1
        if [ "$pf_s" != 0 ]; then
            pf 1 "$pf_what" "docker build failed, its last lines below; fill what it lacked and run --preflight again"
            tail -n 20 "$ENV_DIR/build" | sed 's/^/    | /'
        elif [ "$(env_files || :)" != "$pf_tree" ]; then
            # The label says the files as they were when the build began.
            pf 1 "$pf_what" "a file changed while it built: run --preflight again"
        elif env_image "$pf_tree"; then
            pf 0 "$pf_what (its files $(printf '%.12s' "$pf_tree")), run as $(printf '%.19s' "$ENV_IMAGE"), built now"
        else
            pf 1 "$pf_what" "$ENV_WHY, after its build: run --preflight again"
        fi
        rm -f "$ENV_DIR/build"
    fi
    return "$pf_bad"
}

# env_toys DIR: what a script's self-test runs it with instead of Docker,
# caffeinate, rustup, cargo and ycsb, in DIR, which it must be first on
# the PATH of. Each records what it was asked in DIR. The toy ycsb runs a
# soak or a cell as a loop that writes a line every 0.2 s until it is
# stopped, its pid in DIR/toy.pid -- what a trap that stops `caffeinate`
# alone leaves running -- and a line to its --progress file unless
# TOY_SILENT is set; it lists its --kept file's first volume, `<name>-data`.
# The toy docker's `df` never ends while DIR/hang-df is there, its pid in
# DIR/hung.pid; its `fennecdb-ycsb` is DIR/image, the id and labels as
# `env_image` reads them, which its `build` writes from the labels it is
# given.
env_toys() {
    mkdir -p "$1/bin" "$1/cargo/registry/index"
    cat >"$1/bin/docker" <<'TOY'
#!/bin/sh
echo "$*" >>"$TOY_DIR/docker.log"
case "$1" in
info) echo 68719476736 ;;
volume) [ "$2" = inspect ] && exit 1 ;;
# As Docker does once the soak removed its own network: a cleanup that
# stops at a failing call leaves what comes after it.
network) [ "${2:-}" = rm ] && exit 1 ;;
run)
    case "$*" in
    *" df -kP "*)
        # A call that never ends while DIR/hang-df is there -- a daemon
        # that stopped answering -- its pid where the test reads it.
        if [ -f "$TOY_DIR/hang-df" ]; then
            echo $$ >"$TOY_DIR/hung.pid"
            exec sleep 600
        fi
        printf 'Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/vda1 100000000 0 100000000 0%% /\n'
        ;;
    esac
    ;;
inspect)
    case "$*" in
    *OOMKilled*) echo false ;;
    *ExitCode*) echo 0 ;;
    esac
    ;;
image)
    # Every image by its digest is here; fennecdb-ycsb only once built.
    case "$*" in
    *fennecdb-ycsb*)
        [ -s "$TOY_DIR/image" ] || exit 1
        cat "$TOY_DIR/image"
        ;;
    esac
    ;;
build)
    labels=''
    while [ $# -gt 0 ]; do
        [ "$1" = --label ] && labels="$labels $2"
        shift
    done
    echo "sha256:toybuilt$labels" >"$TOY_DIR/image"
    echo sha256:toybuilt
    ;;
esac
exit 0
TOY
    # caffeinate runs its command as its child and passes no signal on.
    cat >"$1/bin/caffeinate" <<'TOY'
#!/bin/sh
shift
"$@" &
wait $!
TOY
    cat >"$1/bin/rustup" <<'TOY'
#!/bin/sh
case "$1" in
toolchain) sed -n 's/^channel *= *"\([^"]*\)".*/\1-toy/p' rust-toolchain.toml ;;
target) sed -n 's/^targets *= *\[\(.*\)\].*/\1/p' rust-toolchain.toml | tr -d '",' | tr ' ' '\n' ;;
esac
TOY
    cat >"$1/bin/ycsb" <<'TOY'
#!/bin/sh
echo "$*" >>"$TOY_DIR/ycsb.log"
case "$1 ${2:-}" in
"soak report" | mark* | report* | fill* | heap*) exit 0 ;;
esac
case " $* " in
*" --peak "*)
    # A soak stopped on the harness's own fault, as `ycsb soak` exits 4.
    if [ -n "${TOY_STOP:-}" ]; then
        echo "soak: STOPPED, the harness's fault: a toy's" >&2
        exit 4
    fi
    echo "peak 1000"
    exit 0
    ;;
esac
progress='' kept='' name=''
while [ $# -gt 0 ]; do
    case "$1" in
    --progress) progress=$2 ;;
    --kept) kept=$2 ;;
    --name) name=$2 ;;
    esac
    shift
done
[ -n "$kept" ] && echo "$name-data" >>"$kept"
echo $$ >"$TOY_DIR/toy.pid"
while :; do
    echo tick >>"$TOY_DIR/toy.out"
    [ -n "$progress" ] && [ -z "${TOY_SILENT:-}" ] && echo tick >>"$progress"
    sleep 0.2
done
TOY
    chmod +x "$1/bin/docker" "$1/bin/caffeinate" "$1/bin/rustup" "$1/bin/ycsb"
}

# env_toy_run DIR SCRIPT ARGS...: SCRIPT run in the background over the
# toys in DIR, small enough for any disk and under any load, its files in
# DIR too; its pid in `toy_run`.
env_toy_run() {
    tr_dir=$1
    shift
    TOY_DIR=$tr_dir TMPDIR=$tr_dir PATH="$tr_dir/bin:$PATH" CARGO_HOME="$tr_dir/cargo" \
        YCSB="$tr_dir/bin/ycsb" CARGO=true LINUX_TOOLS=true \
        QUIET_LOAD=100000 RECORDS=4000 MEMORY=1m RUNS=1 GAP=0 FRACTIONS="50 80" \
        OUT="$tr_dir/out.tsv" VERIFY="$tr_dir/verify.tsv" RUN=toy \
        "${ENV_SHELL:-sh}" "$@" >>"$tr_dir/run.log" 2>&1 &
    toy_run=$!
}

# env_toy_wait SECS WHAT CMD...: until CMD holds, SECS at the most; 1 and
# WHAT said where it never did. A wait for an event, bounded.
env_toy_wait() {
    tw_left=$(($1 * 10))
    tw_what=$2
    shift 2
    while ! "$@"; do
        tw_left=$((tw_left - 1))
        if [ "$tw_left" -le 0 ]; then
            echo "self-test: $tw_what did not happen" >&2
            return 1
        fi
        sleep 0.1
    done
    return 0
}

# env_self_test SCRIPT SOAK ARGS...: SCRIPT's pre-flight, trap and guard
# held to what they promise, with toys and no Docker (`env_toys`). Its
# `--preflight` builds the image once, labelled with this tree; a run
# starts its servers from that image by its id and builds nothing, and
# refuses one labelled for another tree, starting nothing (so a file
# changed outside crates/fennec-bench while it runs fails it: every run
# hashes the tree as it is then). Interrupted in the
# middle of a step run under `caffeinate`, the script stops the
# measurement itself, not only its parent; where SOAK is not empty, it
# leaves the volumes the soak said to keep and removes its others, and a
# soak that prints nothing is stopped by the guard after ENV_SILENT seconds
# and marked hung. Interrupted in a bounded docker call that never ends --
# its own shell's, before a step, and its guard's, while one runs --
# nothing it started is left running 30 s after: the call's command and
# watch, the guard, the step. Every toy is stopped however the test ends.
# 0 when each held.
env_self_test() {
    st_script=$1
    st_silent=$2
    shift 2
    # A failing check is the test's to say, not `set -e`'s to end it on.
    set +e
    st_dir=$(mktemp -d "${TMPDIR:-/tmp}/ycsb-self-test.XXXXXX")
    st_bad=0
    # shellcheck disable=SC2064
    trap "env_toy_stop '$st_dir'; env_end" EXIT
    trap 'exit 130' INT TERM
    env_toys "$st_dir"
    st() {
        if [ "$1" = 0 ]; then
            echo "self-test ok: $2"
        else
            echo "self-test FAILED: $2"
            st_bad=1
        fi
    }
    # The pre-flight, with no image yet: built, labelled with this tree.
    env_toy_run "$st_dir" "$st_script" --preflight
    st_s=0
    env_toy_ended 60 "the pre-flight's end" || st_s=1
    [ "$st_rc" = 0 ] || st_s=1
    st_files=$(env_files) || st_s=1
    [ "$(grep -c '^build ' "$st_dir/docker.log")" = 1 ] || st_s=1
    grep -q " com.fennecdb.tree=$st_files" "$st_dir/image" || st_s=1
    st "$st_s" "the pre-flight built the image, labelled with this tree's files"
    # Interrupted while its toy measures.
    env_toy_run "$st_dir" "$st_script" "$@"
    env_toy_wait 60 "the toy's first line" test -s "$st_dir/toy.out"
    st_s=$?
    st "$st_s" "the step started under caffeinate"
    st_s=0
    grep -q -- '--image sha256:toybuilt' "$st_dir/ycsb.log" || st_s=1
    st "$st_s" "the run's servers start from the image by its id"
    kill -TERM "$toy_run" 2>/dev/null
    wait "$toy_run" 2>/dev/null
    toy=$(cat "$st_dir/toy.pid" 2>/dev/null)
    st_s=0
    env_toy_wait 10 "the toy's end" env_toy_gone "$toy" || st_s=1
    # One the script left running is the test's to stop.
    [ -n "$toy" ] && kill -TERM "$toy" 2>/dev/null
    st "$st_s" "interrupted, the script stopped the measurement under caffeinate, not only caffeinate"
    if [ -n "$st_silent" ]; then
        st_s=0
        grep -- 'volume rm' "$st_dir/docker.log" | grep -q -- '-50-data' && st_s=1
        grep -- 'volume rm' "$st_dir/docker.log" | grep -q -- '-50-replica-data' || st_s=1
        st "$st_s" "the volume the soak said to keep kept, its others removed"
        st_s=0
        grep -- 'volume rm' "$st_dir/docker.log" | grep -q -- '-tools' || st_s=1
        st "$st_s" "the run's tools volume removed, past a failing docker call"
        # A soak stopped on the harness's own fault: the run ends there in
        # words, no fraction after it started.
        rm -f "$st_dir/toy.pid" "$st_dir/toy.out"
        : >"$st_dir/ycsb.log"
        TOY_STOP=1 env_toy_run "$st_dir" "$st_script" "$@"
        st_s=0
        env_toy_ended 60 "the stopped run's end" || st_s=1
        [ "$st_rc" = 4 ] || st_s=1
        grep -q "stopped on the harness's own fault" "$st_dir/run.log" || st_s=1
        grep -q -- '--rate' "$st_dir/ycsb.log" && st_s=1
        st "$st_s" "a soak stopped on the harness's fault ends the run, nothing after it started"
        rm -f "$st_dir/toy.pid" "$st_dir/toy.out"
        TOY_SILENT=1 ENV_SILENT=3 ENV_EVERY=2 env_toy_run "$st_dir" "$st_script" "$@"
        st_s=0
        env_toy_wait 120 "the silent run's end" env_toy_gone "$toy_run" || st_s=1
        wait "$toy_run" 2>/dev/null
        toy=$(cat "$st_dir/toy.pid" 2>/dev/null)
        env_toy_wait 10 "the silent toy's end" env_toy_gone "$toy" || st_s=1
        [ -n "$toy" ] && kill -TERM "$toy" 2>/dev/null
        grep -q 'printed nothing for' "$st_dir/run.log" || st_s=1
        grep -q ':hung hung 1' "$st_dir/ycsb.log" || st_s=1
        st "$st_s" "a step silent for ENV_SILENT s stopped by the guard and marked hung"
    fi
    # Interrupted in its own shell's docker call, looking at the disks
    # before its first step.
    rm -f "$st_dir/toy.pid" "$st_dir/toy.out" "$st_dir/hung.pid"
    : >"$st_dir/hang-df"
    env_toy_run "$st_dir" "$st_script" "$@"
    st_s=0
    env_toy_wait 60 "the script's own docker call" test -s "$st_dir/hung.pid" || st_s=1
    env_toy_left || st_s=1
    st "$st_s" "interrupted in a docker call of its own, nothing it started left running"
    # Interrupted while its guard is in one, a step running.
    rm -f "$st_dir/toy.pid" "$st_dir/toy.out" "$st_dir/hung.pid" "$st_dir/hang-df"
    ENV_EVERY=2 env_toy_run "$st_dir" "$st_script" "$@"
    st_s=0
    env_toy_wait 60 "the toy's first line" test -s "$st_dir/toy.out" || st_s=1
    : >"$st_dir/hang-df"
    env_toy_wait 30 "the guard's docker call" test -s "$st_dir/hung.pid" || st_s=1
    env_toy_left || st_s=1
    st "$st_s" "interrupted while its guard was in a docker call, nothing it started left running"
    rm -f "$st_dir/toy.pid" "$st_dir/hung.pid" "$st_dir/hang-df"
    # None of the runs built: the pre-flight's build is the only one.
    st_s=0
    [ "$(grep -c '^build ' "$st_dir/docker.log")" = 1 ] || st_s=1
    st "$st_s" "no run built an image"
    # An image labelled for another tree: refused in words, nothing built,
    # nothing measured.
    echo "sha256:toyother com.fennecdb.commit=0123456789ab com.fennecdb.tree=0123456789abcdef0123456789abcdef01234567" \
        >"$st_dir/image"
    : >"$st_dir/ycsb.log"
    rm -f "$st_dir/toy.out"
    env_toy_run "$st_dir" "$st_script" "$@"
    st_s=0
    env_toy_ended 60 "the refused run's end" || st_s=1
    [ "$st_rc" = 2 ] || st_s=1
    grep -q 'MISSING  fennecdb-ycsb built from this tree: built from another tree' "$st_dir/run.log" || st_s=1
    [ "$(grep -c '^build ' "$st_dir/docker.log")" = 1 ] || st_s=1
    [ -s "$st_dir/ycsb.log" ] && st_s=1
    [ -s "$st_dir/toy.out" ] && st_s=1
    st "$st_s" "an image labelled for another tree refused, nothing built or started"
    env_toy_stop "$st_dir"
    env_end
    trap - EXIT INT TERM
    [ "$st_bad" = 0 ] && echo "self-test: every promise held" || echo "self-test: FAILED, its log in the run's output above"
    return "$st_bad"
}

env_toy_gone() {
    [ -z "$1" ] || ! kill -0 "$1" 2>/dev/null
}

# env_toy_ended SECS WHAT: the toy run's end, SECS at the most, its exit
# status in st_rc; 1 and WHAT said where it did not end -- a run that went
# on to measure, which is then stopped as `kill <its pid>` does, with
# everything under it, rather than waited for.
env_toy_ended() {
    st_rc=0
    if env_toy_wait "$1" "$2" env_toy_gone "$toy_run"; then
        wait "$toy_run" 2>/dev/null || st_rc=$?
        return 0
    fi
    st_rc=143
    env_toy_left
    return 1
}

# Whether PID runs: a zombie, ended and not yet waited for, does not.
env_toy_alive() {
    kill -0 "$1" 2>/dev/null || return 1
    case $(ps -o stat= -p "$1" 2>/dev/null) in
    Z*) return 1 ;;
    esac
    return 0
}

env_toy_all_gone() {
    for ag_p in "$@"; do
        env_toy_alive "$ag_p" && return 1
    done
    return 0
}

# env_tree PID: PID and every process under it, a pid a line.
env_tree() {
    echo "$1"
    for et_c in $(pgrep -P "$1" 2>/dev/null); do
        env_tree "$et_c"
    done
}

# env_toy_left: the toy run interrupted as `kill <its pid>` does, and every
# process under it then -- its step, its guard, a docker call and the watch
# beside it -- gone within 30 s; 1 where one was not, each such one named
# and stopped (`st_tree` holds them for the trap meanwhile).
env_toy_left() {
    st_tree=$(env_tree "$toy_run")
    kill -TERM "$toy_run" 2>/dev/null
    tl_s=0
    # shellcheck disable=SC2086 # the pids, a word each
    env_toy_wait 30 "the interrupted run's end" env_toy_all_gone $st_tree || tl_s=1
    for tl_p in $st_tree; do
        if env_toy_alive "$tl_p"; then
            echo "self-test: left running: $(ps -o pid=,command= -p "$tl_p" 2>/dev/null)" >&2
            kill -KILL "$tl_p" 2>/dev/null
        fi
    done
    wait "$toy_run" 2>/dev/null
    st_tree=
    return "$tl_s"
}

# Whatever the test started, stopped -- the script, its toy loop, a toy
# docker call that hangs, an interrupted run's processes -- and its
# directory removed, after printing the toy run's log where one failed.
env_toy_stop() {
    if [ -n "${toy_run:-}" ]; then
        kill -TERM "$toy_run" 2>/dev/null
    fi
    if [ -s "$1/toy.pid" ]; then
        kill -TERM "$(cat "$1/toy.pid")" 2>/dev/null
    fi
    if [ -s "$1/hung.pid" ]; then
        kill -TERM "$(cat "$1/hung.pid")" 2>/dev/null
    fi
    for ts_p in ${st_tree:-}; do
        kill -KILL "$ts_p" 2>/dev/null
    done
    if [ "${st_bad:-0}" != 0 ] && [ -f "$1/run.log" ]; then
        sed 's/^/  | /' "$1/run.log"
    fi
    case $1 in
    */ycsb-self-test.*) rm -rf "$1" ;;
    esac
}
