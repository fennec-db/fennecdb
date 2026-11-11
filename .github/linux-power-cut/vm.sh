#!/usr/bin/env bash
# The host's side of the Linux power-cut test (docs/design/durability.md,
# "A power cut"): an Ubuntu cloud image under QEMU/KVM on a GitHub-hosted
# runner, set up by cloud-init, reached over SSH and HTTP through QEMU's
# port forwards, running this commit's fennec-server under `--sync always`
# on a data disk of its own ($FS, ext4 unless told otherwise), and cut by
# `kill -9` of QEMU while client.py writes to it -- the guest's memory, its
# page cache and the file system's unwritten journal go with it; what the
# guest wrote to its disks (cache.direct=on: O_DIRECT on the host) stays. A
# cut marked `lost` also rebuilds the data disk as of the last flush the
# guest asked for (blklogwrites, replay.py): a disk whose volatile write
# cache was lost, which is what `--sync always` and the sync log's fdatasync
# must survive. Windows' rig (.github/windows-power-cut/vm.sh) is this
# one's counterpart; the load, the check and the replay are both's
# (.github/power-cut/).
#
#   vm.sh boot           the cloud image's first boot, set up by cloud-init,
#                        then a clean restart
#   vm.sh fennec         this run's fennec-server from the job beside this
#                        one into the guest, the token, the data disk made
#                        and mounted at /d, the server started, the
#                        collections
#   vm.sh cut N MODE NEXT  the load, a kill -9 at a random time in the
#                        window, the boot, the data disk mounted, the server
#                        on the same file, the check. MODE plain: the disk
#                        as QEMU left it; lost: that, then the disk as of its
#                        last flush, which the next cuts go on from. NEXT
#                        log: the boot after this cut logs the data disk's
#                        writes, for a lost cut next.
#   vm.sh summary        what every step found, for the run's summary page
set -euo pipefail
VM=${VM:?}
HERE=$(cd "$(dirname "$0")" && pwd)
SHARED=$(cd "$HERE/../power-cut" && pwd)
SEED=${SEED:-0}
FS=${FS:-ext4}
WINDOW_LO=${WINDOW_LO:-2}
WINDOW_HI=${WINDOW_HI:-30}
# The data disk, by the serial QEMU gives it: /dev/vdb as long as the root
# disk comes first, but named so it is never taken for another.
DATA=/dev/disk/by-id/virtio-fennecd
SSHO=(-i "$VM/key" -o IdentitiesOnly=yes -o BatchMode=yes -o StrictHostKeyChecking=no
      -o UserKnownHostsFile=/dev/null -o ConnectTimeout=10 -o LogLevel=ERROR)
gssh() { ssh "${SSHO[@]}" -p 2222 fennec@127.0.0.1 "$@"; }
gscp() { scp "${SSHO[@]}" -P 2222 "$@"; }
now() { date +%s; }
CLIENT=(python3 "$SHARED/client.py" --token-file "$VM/token" --state "$VM/state")

# A line for the log and for the run's summary page.
say() { echo "$*"; echo "$*" >> "$VM/summary.txt"; }

qemu_alive() { kill -0 "$(cat "$VM/qemu.pid" 2>/dev/null)" 2>/dev/null; }

serial_tail() {
    echo "== the serial log's last 80 lines"
    tail -n 80 "$VM/serial.log" 2>/dev/null | tr -d '\r' || true
}

# Killed and gone: QEMU's own exit leaves nothing to wait for but the pid.
qemu_kill() {
    kill -9 "$(cat "$VM/qemu.pid")" 2>/dev/null || true
    for _ in $(seq 1 50); do qemu_alive || return 0; sleep 0.2; done
    echo "QEMU still there 10 s after kill -9"
    return 1
}

# This boot's id, as the guest's kernel says; nothing while it is down.
boot_id() {
    timeout 20 ssh "${SSHO[@]}" -p 2222 fennec@127.0.0.1 'cat /proc/sys/kernel/random/boot_id' 2>/dev/null
}

# $1 the data disk (qcow2); $2 a file to log its writes to, or empty. Both
# disks are virtio disks with a write cache the guest must flush
# (write-cache=on: the guest's /sys/block/vd?/queue/write_cache says "write
# back"), each image opened O_DIRECT on the host, so a guest's flush is the
# host's fdatasync. Only the data disk is ever logged: blklogwrites is a
# filter between it and its image, appending each write and each flush to
# $2 as it completes (block/blklogwrites.c). Its log needs no flush of its
# own -- kill -9 leaves the host's page cache whole. The root disk keeps
# what the guest wrote to it, a cut after a cut; cloud-init's seed rides
# along at every boot, so no boot is taken for a new instance.
start_vm() {
    local disk=$1 log=$2
    rm -f "$VM/qemu.pid"
    wc -l < "$VM/serial.log" > "$VM/serial.mark" 2>/dev/null || echo 0 > "$VM/serial.mark"
    local drive=(-blockdev "driver=file,node-name=rf,filename=$VM/root.qcow2,cache.direct=on"
                 -blockdev "driver=qcow2,node-name=r0,file=rf"
                 -device "virtio-blk-pci,drive=r0,bootindex=0,write-cache=on"
                 -blockdev "driver=file,node-name=f0,filename=$disk,cache.direct=on")
    if [ -n "$log" ]; then
        : > "$log"
        drive+=(-blockdev "driver=qcow2,node-name=q0,file=f0"
                -blockdev "driver=file,node-name=l0,filename=$log,cache.no-flush=on"
                -blockdev "driver=blklogwrites,node-name=d0,file=q0,log=l0,log-sector-size=512")
    else
        drive+=(-blockdev "driver=qcow2,node-name=d0,file=f0")
    fi
    # No merging of adjacent requests into one: virtio-blk merges them up
    # to IOV_MAX buffers, and blklogwrites writes a request's log entry as
    # its buffers and two of its own, which pwritev refuses past IOV_MAX --
    # run 43010736756's guest got EIO for four merged requests of 256
    # buffers each, and its log a hole where their entry was.
    drive+=(-device "virtio-blk-pci,drive=d0,serial=fennecd,write-cache=on,request-merging=off")
    qemu-system-x86_64 -machine q35,accel=kvm -cpu host -smp 4 -m 4096 \
        "${drive[@]}" \
        -drive if=none,id=cd0,file="$VM/seed.iso",format=raw,media=cdrom,readonly=on \
        -device ide-cd,drive=cd0,bus=ide.1 \
        -nic user,model=virtio-net-pci,hostfwd=tcp:127.0.0.1:2222-:22,hostfwd=tcp:127.0.0.1:8080-:8080 \
        -chardev file,id=com1,path="$VM/serial.log",append=on -serial chardev:com1 \
        -display none -rtc base=utc \
        -pidfile "$VM/qemu.pid" -daemonize
    echo "qemu pid $(cat "$VM/qemu.pid"): data disk $(basename "$disk")${log:+, its writes logged to $(basename "$log")}"
}

# The I/O errors the guest's kernel logged since this boot began: a request
# QEMU failed, which is the rig's fault and never the server's, and which
# leaves a hole in a write log besides.
disk_errors() {
    local from
    from=$(cat "$VM/serial.mark" 2>/dev/null || echo 0)
    tail -n +"$((from + 1))" "$VM/serial.log" 2>/dev/null | tr -d '\r' |
        grep -E 'I/O error, dev vd|Buffer I/O error|EXT4-fs error|XFS \(vd.\).*(error|I/O)' || true
}

# Until a command runs over SSH, bounded by $1 seconds.
wait_ssh() {
    local limit=$1 tag=$2 t0 el
    t0=$(now)
    while :; do
        el=$(( $(now) - t0 ))
        if ! qemu_alive; then echo "QEMU exited after ${el}s ($tag)"; serial_tail; return 1; fi
        if timeout 20 ssh "${SSHO[@]}" -p 2222 fennec@127.0.0.1 true 2>/dev/null; then
            echo "ssh up after ${el}s ($tag)"
            return 0
        fi
        if (( el > limit )); then echo "no ssh after ${el}s ($tag)"; serial_tail; return 1; fi
        sleep 2
    done
}

boot() {
    mkdir -p "$VM/state" "$VM/seed"
    # This run's key, the guest's only way in: made here, never uploaded.
    ssh-keygen -q -t ed25519 -N '' -C power-cut -f "$VM/key"
    sed "s|@KEY@|$(cat "$VM/key.pub")|" "$HERE/user-data" > "$VM/seed/user-data"
    printf 'instance-id: fennec-power-cut\nlocal-hostname: fennec-cut\n' > "$VM/seed/meta-data"
    genisoimage -quiet -J -r -V cidata -o "$VM/seed.iso" "$VM/seed/user-data" "$VM/seed/meta-data"
    qemu-img create -q -f qcow2 -b "$VM/cloud.img" -F qcow2 "$VM/root.qcow2" 12G
    qemu-img create -q -f qcow2 "$VM/data.qcow2" 8G
    echo "$VM/data.qcow2" > "$VM/cur"
    local t0 tr booted again=
    t0=$(now)
    start_vm "$VM/data.qcow2" ""
    wait_ssh 600 first-boot
    for _ in $(seq 1 150); do
        gssh 'test -e /var/lib/power-cut-ready' 2>/dev/null && break
        qemu_alive || { echo "QEMU exited during cloud-init"; serial_tail; return 1; }
        sleep 2
    done
    if ! gssh 'test -e /var/lib/power-cut-ready'; then
        echo "cloud-init did not finish"
        gssh 'cloud-init status --long; sudo tail -n 60 /var/log/cloud-init-output.log' || true
        return 1
    fi
    say "TIMING first_boot_to_ready=$(( $(now) - t0 ))s"
    say "GUEST $(gssh '. /etc/os-release; echo "$PRETTY_NAME, kernel $(uname -r), $(nproc) cpus, $(free -m | awk "/^Mem:/ {print \$2}") MiB"')"
    # A clean restart before any cut: what cloud-init set (GRUB's
    # recordfail off, the timers off) on disk, and the boot a cut's boot
    # will be, held to a new boot id.
    booted=$(boot_id)
    tr=$(now)
    timeout 30 ssh "${SSHO[@]}" -p 2222 fennec@127.0.0.1 'sudo systemctl reboot' >/dev/null 2>&1 || true
    for _ in $(seq 1 150); do
        again=$(boot_id || true)
        [ -n "$again" ] && [ "$again" != "$booted" ] && break
        qemu_alive || { echo "QEMU exited during the restart"; serial_tail; return 1; }
        sleep 2
    done
    if [ -z "$again" ] || [ "$again" = "$booted" ]; then
        echo "the guest did not restart: its boot id is $booted still"
        serial_tail
        return 1
    fi
    say "TIMING clean_restart_to_first_command=$(( $(now) - tr ))s"
}

# The data disk at /d, as no boot mounts it itself: the file system's own
# recovery after a cut (ext4's journal replayed) is this mount's, its
# kernel lines kept in the summary.
mount_data() {
    local what=$1 dev
    if ! gssh "sudo mkdir -p /d && sudo mount -t $FS $DATA /d"; then
        echo "MOUNT $what: the data disk did not mount"
        gssh 'sudo dmesg | tail -n 60' || true
        return 1
    fi
    dev=$(gssh "basename \"\$(readlink -f $DATA)\"")
    gssh 'sudo dmesg' | grep -E "(EXT4-fs|XFS) \($dev\)" | sed "s/^/MOUNT $what: /" |
        while read -r l; do say "$l"; done || true
}

# What the file held when a server could not open it or its check failed,
# into a log the run uploads (dump.sh): from the copy taken before the
# server opened it, where there is one. Never fails the step itself.
file_dump() {
    local tag=$1 dir=${2:-/d} at
    at=$(grep -oE ' at [0-9]+' "$VM/server-$tag.log" | tail -1 | grep -oE '[0-9]+' || true)
    gssh "bash -s -- '$at' '$dir'" < "$HERE/dump.sh" > "$VM/server-$tag-file.log" 2>&1 || true
    echo "FILE what the server found: $VM/server-$tag-file.log"
}

# The files a cut left beside the database, as they were before the server
# opened them -- the copy boot_check takes -- into the run's uploaded logs:
# what a failure is read from. Run 42533808035 of the Windows rig kept no
# file and its cause was read from the error alone.
keep() {
    local tag=$1
    mkdir -p "$VM/evidence"
    gssh 'tar -C pre -czf - .' > "$VM/evidence/files-$tag.tar.gz" 2>/dev/null || true
    say "KEPT the files cut $tag left, before the server opened them: evidence/files-$tag.tar.gz, $(stat -c %s "$VM/evidence/files-$tag.tar.gz") bytes"
    file_dump "$tag" pre
}

# The server over a session of its own, its output the host's file: the
# session ends with QEMU. The token comes from the file the host copied in,
# never the command line (argv shows up in a process listing) nor the log.
serve() {
    local tag=$1 t0 code sp
    t0=$(now)
    setsid ssh "${SSHO[@]}" -p 2222 fennec@127.0.0.1 \
        'FENNEC_HTTP_TOKEN=$(cat ci/token); export FENNEC_HTTP_TOKEN; exec ci/fennec-server --file /d/t.fennec --http 0.0.0.0:8080 --sync always' \
        > "$VM/server-$tag.log" 2>&1 < /dev/null &
    sp=$!
    for _ in $(seq 1 120); do
        if ! kill -0 "$sp" 2>/dev/null; then
            echo "SERVER $tag exited before it answered"
            cat "$VM/server-$tag.log"
            return 1
        fi
        code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 5 http://127.0.0.1:8080/_health || true)
        if [ "$code" = 200 ]; then
            say "SERVER $tag answered /_health $(( $(now) - t0 ))s after its start"
            sed "s/^/SERVER $tag: /" "$VM/server-$tag.log"
            grep -q '^opened: ' "$VM/server-$tag.log" || { echo "no \`opened:\` line"; return 1; }
            return 0
        fi
        sleep 1
    done
    echo "SERVER $tag: no answer from /_health in 120 s"
    cat "$VM/server-$tag.log"
    return 1
}

fennec() {
    local t0 ok=
    t0=$(now)
    for _ in $(seq 1 160); do
        if gh run download "$GITHUB_RUN_ID" -R "$GITHUB_REPOSITORY" -n fennec-linux-server -D "$VM/bin" >/dev/null 2>&1; then
            ok=1
            break
        fi
        if [ "$(gh run view "$GITHUB_RUN_ID" -R "$GITHUB_REPOSITORY" --json jobs \
                -q '.jobs[] | select(.name | startswith("fennec-server")) | .conclusion' 2>/dev/null)" = failure ]; then
            echo "fennec-server's build failed"
            return 1
        fi
        sleep 15
    done
    [ -n "$ok" ] || { echo "no fennec-server after $(( $(now) - t0 ))s"; return 1; }
    say "TIMING server_build_wait=$(( $(now) - t0 ))s"
    say "BINARY fennec-server $(wc -c < "$VM/bin/fennec-server") bytes, sha256 $(sha256sum "$VM/bin/fennec-server" | cut -c1-16)"
    gssh 'mkdir -p -m 700 ci'
    gscp "$VM/bin/fennec-server" "$VM/token" fennec@127.0.0.1:ci/
    gssh 'chmod 755 ci/fennec-server && chmod 600 ci/token'
    say "FENNEC $(gssh 'ci/fennec-server --version')"
    # The data disk, made whole now: no lazy init writing the inode tables
    # or the journal behind the cuts.
    case $FS in
        ext4) gssh "sudo mkfs.ext4 -q -F -L fennecd -E lazy_itable_init=0,lazy_journal_init=0 $DATA" ;;
        xfs) gssh "sudo mkfs.xfs -q -f -L fennecd $DATA" ;;
        *) echo "no mkfs for $FS"; return 1 ;;
    esac
    mount_data setup
    gssh 'sudo chown fennec:fennec /d'
    say "DISK $(gssh "dev=\$(basename \"\$(readlink -f $DATA)\"); echo \"\$dev: write_cache \$(cat /sys/block/\$dev/queue/write_cache), \$(findmnt -no FSTYPE,OPTIONS /d)\"")"
    # The rig's own files on disk before any cut: the Windows rig's run 4
    # lost a token copied in a minute before its first cut.
    gssh sync
    serve 0
    "${CLIENT[@]}" setup
}

# The server that opened the file after the cut holds its lock: a second
# one over the file is refused, naming it. Bounded: one that is not refused
# would serve.
lock_held() {
    local out
    out=$(timeout 60 ssh "${SSHO[@]}" -p 2222 fennec@127.0.0.1 \
        'ci/fennec-server --file /d/t.fennec --http 127.0.0.1:0' 2>&1 || true)
    if ! grep -q 'it is open already' <<< "$out"; then
        echo "LOCK $1: a second server over the file was not refused: $out"
        return 1
    fi
    say "LOCK $1: a second server over the file refused: $out"
}

# The data disk as of the last flush cut N's log holds: an overlay on the
# image the logged boot began from, the log replayed into it through
# qemu-nbd.
lost_disk() {
    local n=$1 base lost np t0
    base=$(cat "$VM/base-$n")
    lost=$VM/lost-$n.qcow2
    qemu-img create -q -f qcow2 -b "$base" -F qcow2 "$lost"
    rm -f "$VM/nbd.sock"
    qemu-nbd --socket="$VM/nbd.sock" --format=qcow2 --cache=writeback "$lost" &
    np=$!
    for _ in $(seq 1 100); do [ -S "$VM/nbd.sock" ] && break; sleep 0.1; done
    t0=$(now)
    python3 "$SHARED/replay.py" "$VM/log-$n.bin" "$VM/nbd.sock" | tee "$VM/replay-$n.log"
    wait "$np"
    say "$(grep '^REPLAY [0-9]' "$VM/replay-$n.log" | sed "s/^REPLAY/REPLAY cut $n:/")"
    say "TIMING cut${n}_replay=$(( $(now) - t0 ))s log=$(( $(stat -c %s "$VM/log-$n.bin") >> 20 ))MiB"
    rm -f "$VM/log-$n.bin"
}

# Boot with data disk $3 (logging to $4 when given), the disk mounted, the
# server on the same file, the check of cut $1 as $2, kept as what the next
# cut is held to with $5.
boot_check() {
    local n=$1 label=$2 disk=$3 log=$4 commit=${5:-} t0
    t0=$(now)
    start_vm "$disk" "$log"
    wait_ssh 300 "boot$n-$label"
    say "TIMING cut${n}_${label}_boot_to_first_command=$(( $(now) - t0 ))s"
    mount_data "cut $n $label"
    # What the cut left beside the file: its lock, the sync log, and the
    # image a compact it stopped was writing (`.beside` for a compact beside
    # the database, `.compacting` for one under the lock), which the next
    # compact writes anew; ext4's own lost+found.
    gssh 'ls -A /d' > "$VM/files-$n-$label.txt"
    say "FILES cut $n $label: $(tr '\n' ' ' < "$VM/files-$n-$label.txt")"
    if grep -v -x -E 'lost\+found|t\.fennec|t\.fennec\.(lock|sync|beside|compacting)' "$VM/files-$n-$label.txt"; then
        echo "a file the server does not keep is beside the database"
        return 1
    fi
    if ! gssh 'test -s ci/token && test -x ci/fennec-server'; then
        echo "the rig's own files did not survive the cut: the token or fennec-server in ~/ci"
        return 1
    fi
    # The files as the cut left them, before the server's open applies the
    # sync log or cuts a torn tail: kept if anything after fails.
    gssh 'rm -rf pre && mkdir -m 700 pre && cp -a /d/t.fennec* pre/'
    say "SYNCLOG cut $n $label: $(gssh 'python3 - /d/t.fennec /d/t.fennec.sync' < "$HERE/synclog.py")"
    if ! serve "$n-$label"; then
        keep "$n-$label"
        return 1
    fi
    lock_held "$n-$label"
    t0=$(now)
    if ! "${CLIENT[@]}" check --cut "$n" --label "$label" --cut-time "$(tr '\n' ' ' < "$VM/cut-$n.time")" $commit \
            > "$VM/check-$n-$label.log" 2>&1; then
        cat "$VM/check-$n-$label.log"
        grep -E '^(VERDICT|BAD)' "$VM/check-$n-$label.log" >> "$VM/summary.txt" || true
        keep "$n-$label"
        return 1
    fi
    cat "$VM/check-$n-$label.log"
    grep -E '^(VERDICT|DOCS|CHURN|SPILL|IDEM|LOAD|BATCH|INFLIGHT|PROBE|WARM)' "$VM/check-$n-$label.log" >> "$VM/summary.txt"
    say "TIMING cut${n}_${label}_check=$(( $(now) - t0 ))s"
}

power_cut() {
    local n=$1 mode=$2 next=$3 lp delay cur base log errs
    cur=$(cat "$VM/cur")
    if [ "$mode" = lost ] && [ ! -f "$VM/log-$n.bin" ]; then
        echo "cut $n is to lose the disk's cache, and its boot logged no writes"
        return 1
    fi
    "${CLIENT[@]}" load --cut "$n" --seed "$SEED" > "$VM/load-$n.log" 2>&1 &
    lp=$!
    for _ in $(seq 1 120); do
        grep -q '"ev": "ack"' "$VM/state/events-$n.jsonl" 2>/dev/null && break
        sleep 0.5
    done
    if ! grep -q '"ev": "ack"' "$VM/state/events-$n.jsonl" 2>/dev/null; then
        echo "no write acknowledged in 60 s"
        cat "$VM/load-$n.log"
        kill "$lp" 2>/dev/null || true
        return 1
    fi
    delay=$(python3 -c 'import random, sys; print(f"{random.Random(sys.argv[1]).uniform(float(sys.argv[2]), float(sys.argv[3])):.3f}")' \
        "$SEED-$n" "$WINDOW_LO" "$WINDOW_HI")
    say "CUT $n ($mode) ${delay}s into the writes (seed $SEED, a window of ${WINDOW_LO} to ${WINDOW_HI}s)"
    sleep "$delay"
    date +%s.%N > "$VM/cut-$n.time"
    kill -9 "$(cat "$VM/qemu.pid")"
    date +%s.%N >> "$VM/cut-$n.time"
    qemu_kill
    # The load's threads each stop at their first error, which the cut is.
    for _ in $(seq 1 120); do kill -0 "$lp" 2>/dev/null || break; sleep 0.5; done
    kill -9 "$lp" 2>/dev/null || true
    wait "$lp" 2>/dev/null || true
    errs=$(disk_errors)
    if [ -n "$errs" ]; then
        say "RIG the guest's disks failed requests in the boot before cut $n: QEMU's fault, not the server's"
        head -n 20 <<< "$errs"
        return 1
    fi
    if [ "$mode" = lost ]; then
        lost_disk "$n"
        boot_check "$n" plain "$cur" ""
        qemu_kill
        cur=$VM/lost-$n.qcow2
    fi
    log=
    if [ "$next" = log ]; then
        base=$cur
        echo "$base" > "$VM/base-$((n + 1))"
        cur=$VM/run-$((n + 1)).qcow2
        qemu-img create -q -f qcow2 -b "$base" -F qcow2 "$cur"
        log=$VM/log-$((n + 1)).bin
    fi
    echo "$cur" > "$VM/cur"
    boot_check "$n" "$mode" "$cur" "$log" --commit
}

summary() {
    cat "$VM/summary.txt"
    if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
        { echo '```'; cat "$VM/summary.txt"; echo '```'; } >> "$GITHUB_STEP_SUMMARY"
    fi
}

case ${1:-} in
    boot | fennec | summary) "$1" ;;
    cut) shift; power_cut "$@" ;;
    *) echo "usage: vm.sh boot | fennec | cut N plain|lost plain|log | summary" >&2; exit 2 ;;
esac
