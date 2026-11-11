#!/usr/bin/env bash
# The host's side of the Windows power-cut test (docs/design/windows.md,
# decision 9): a Windows Server 2025 evaluation guest under QEMU/KVM on a
# GitHub-hosted runner, reached over SSH and HTTP through QEMU's port
# forwards, running the release's fennec-server.exe under `--sync always`,
# and cut by `kill -9` of QEMU while client.py writes to it -- the guest's
# memory, Windows' cache and NTFS's unwritten log go with it; what the guest
# wrote to its disk (cache.direct=on: O_DIRECT on the host) stays. A cut
# marked `lost` also rebuilds the disk as of the last flush the guest asked
# for (blklogwrites, replay.py): a disk whose volatile write cache was lost,
# which is what `--sync always` and FlushFileBuffers must survive.
#
#   vm.sh install        install from $VM/win.iso, unattended, to the first
#                        command, then a clean restart
#   vm.sh fennec         the release's zip from this run's other job into the
#                        guest, the token, the server started, the collections
#   vm.sh cut N MODE NEXT  the load, a kill -9 at a random time in the
#                        window, the boot, the server on the same file, the
#                        check. MODE plain: the disk as QEMU left it; lost:
#                        that, then the disk as of its last flush, which the
#                        next cuts go on from. NEXT log: the boot after this
#                        cut logs its disk's writes, for a lost cut next.
#   vm.sh summary        what every step found, for the run's summary page
set -euo pipefail
VM=${VM:?}
HERE=$(cd "$(dirname "$0")" && pwd)
# The host's files Linux's rig shares: the load and check, and the replay.
SHARED=$(cd "$HERE/../power-cut" && pwd)
PW='Fennec-PowerCut-2026!'
SEED=${SEED:-0}
WINDOW_LO=${WINDOW_LO:-2}
WINDOW_HI=${WINDOW_HI:-30}
SSHO=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=10
      -o LogLevel=ERROR -o PubkeyAuthentication=no
      -o PreferredAuthentications=password,keyboard-interactive)
gssh() { sshpass -p "$PW" ssh "${SSHO[@]}" -p 2222 Administrator@127.0.0.1 "$@"; }
gscp() { sshpass -p "$PW" scp "${SSHO[@]}" -P 2222 "$@"; }
now() { date +%s; }
T0=$(now)
CLIENT=(python3 "$SHARED/client.py" --token-file "$VM/token" --state "$VM/state")

# A line for the log and for the run's summary page.
say() { echo "$*"; echo "$*" >> "$VM/summary.txt"; }

shot() {
    printf '{"execute":"qmp_capabilities"}\n{"execute":"screendump","arguments":{"filename":"%s","format":"png"}}\n' \
        "$VM/shots/$1.png" | timeout 10 socat -t 3 - UNIX-CONNECT:"$VM/qmp.sock" >/dev/null 2>&1 || true
}

# The serial lines not shown yet; the offset is kept in a file, the log
# appended by every boot.
serial_new() {
    local seen n
    seen=$(cat "$VM/serial.seen" 2>/dev/null || echo 0)
    n=$(wc -l < "$VM/serial.log" 2>/dev/null || echo 0)
    if [ "$n" -gt "$seen" ]; then
        sed -n "$((seen + 1)),${n}p" "$VM/serial.log" | tr -d '\r' | sed "s/^/serial +$(( $(now) - T0 ))s: /"
        echo "$n" > "$VM/serial.seen"
    fi
}

qemu_alive() { kill -0 "$(cat "$VM/qemu.pid" 2>/dev/null)" 2>/dev/null; }

# When the guest last booted, as Windows says; nothing while it is down.
boot_time() {
    timeout 30 sshpass -p "$PW" ssh "${SSHO[@]}" -p 2222 Administrator@127.0.0.1 \
        'powershell -NoProfile -Command "(Get-CimInstance Win32_OperatingSystem).LastBootUpTime.ToString(\"s\")"' 2>/dev/null |
        tr -d '\r' | grep -E '^[0-9]{4}-'
}

# Killed and gone: QEMU's own exit leaves nothing to wait for but the pid.
qemu_kill() {
    kill -9 "$(cat "$VM/qemu.pid")" 2>/dev/null || true
    for _ in $(seq 1 50); do qemu_alive || return 0; sleep 0.2; done
    echo "QEMU still there 10 s after kill -9"
    return 1
}

# $1 the disk (qcow2); $2 a file to log its writes to, or empty; the rest
# goes to QEMU. The disk is the same AHCI disk with a write cache the guest
# must flush (write-cache=on, as `cache=none` gave the install), logged or
# not: blklogwrites is a filter between it and the image, appending each
# write and each flush to $2 as it completes (block/blklogwrites.c). Its log
# needs no flush of its own -- kill -9 leaves the host's page cache whole.
start_vm() {
    local disk=$1 log=$2
    shift 2
    rm -f "$VM/qemu.pid" "$VM/qmp.sock"
    local drive=(-blockdev "driver=file,node-name=f0,filename=$disk,cache.direct=on")
    if [ -n "$log" ]; then
        : > "$log"
        drive+=(-blockdev "driver=qcow2,node-name=q0,file=f0"
                -blockdev "driver=file,node-name=l0,filename=$log,cache.no-flush=on"
                -blockdev "driver=blklogwrites,node-name=d0,file=q0,log=l0,log-sector-size=512")
    else
        drive+=(-blockdev "driver=qcow2,node-name=d0,file=f0")
    fi
    qemu-system-x86_64 -machine q35,accel=kvm \
        -cpu host,hv_relaxed,hv_spinlocks=0x1fff,hv_vapic,hv_time -smp 4 -m 6144 \
        "${drive[@]}" -device ide-hd,drive=d0,bus=ide.0,bootindex=0,write-cache=on \
        "$@" \
        -nic user,model=e1000e,hostfwd=tcp:127.0.0.1:2222-:22,hostfwd=tcp:127.0.0.1:8080-:8080 \
        -chardev file,id=com1,path="$VM/serial.log",append=on -serial chardev:com1 \
        -display none -vga std -rtc base=utc \
        -qmp unix:"$VM/qmp.sock",server=on,wait=off \
        -pidfile "$VM/qemu.pid" -daemonize
    echo "qemu pid $(cat "$VM/qemu.pid"): $(basename "$disk")${log:+, its writes logged to $(basename "$log")}"
}

# Until a command runs over SSH: a screenshot every $2 seconds, the serial
# log as it grows. $1 bounds the wait; a screen unchanged for $4 seconds is a
# stuck guest, not a slow one. With $5, a screen unchanged for that long gets
# one Enter (logged MANUAL: what a person at the console would have to do).
wait_ssh() {
    local limit=$1 every=$2 tag=$3 stuck=$4 press=${5:-} el last=-999999 shot_now prev= sum same_sum= same_since=0 pressed=
    T0=$(now)
    while :; do
        el=$(( $(now) - T0 ))
        serial_new
        if ! qemu_alive; then echo "QEMU exited after ${el}s"; return 1; fi
        if (( el - last >= every )); then
            # The previous shot is hashed: QEMU may still be writing this one.
            if [ -n "$prev" ]; then
                sum=$(md5sum < "$prev" 2>/dev/null | cut -c1-32 || true)
                if [ -n "$sum" ] && [ "$sum" = "$same_sum" ]; then
                    if [ -n "$press" ] && [ -z "$pressed" ] && (( last - same_since >= press )); then
                        say "MANUAL Enter pressed at ${el}s of $tag: screen unchanged since ${same_since}s ($(basename "$prev"))"
                        printf '{"execute":"qmp_capabilities"}\n{"execute":"send-key","arguments":{"keys":[{"type":"qcode","data":"ret"}]}}\n' |
                            timeout 10 socat -t 3 - UNIX-CONNECT:"$VM/qmp.sock" >/dev/null 2>&1 || true
                        pressed=1
                    elif (( last - same_since >= stuck )); then
                        echo "screen unchanged since ${same_since}s, at ${el}s ($(basename "$prev"))"
                        return 1
                    fi
                else
                    same_sum=$sum
                    same_since=$last
                fi
            fi
            shot_now="$VM/shots/$tag-$(printf %05d "$el").png"
            shot "$(basename "$shot_now" .png)"
            prev=$shot_now
            last=$el
        fi
        if timeout 30 sshpass -p "$PW" ssh "${SSHO[@]}" -p 2222 Administrator@127.0.0.1 "echo ok" 2>/dev/null | grep -q ok; then
            echo "ssh up after ${el}s"
            return 0
        fi
        if (( el > limit )); then echo "no ssh after ${el}s"; shot "$tag-timeout"; return 1; fi
        sleep 5
    done
}

install() {
    mkdir -p "$VM/shots" "$VM/cfg" "$VM/state"
    cp "$HERE/autounattend.xml" "$HERE/firstlogon.ps1" "$VM/cfg/"
    genisoimage -quiet -J -joliet-long -r -V CONFIG -o "$VM/config.iso" "$VM/cfg"
    qemu-img create -q -f qcow2 "$VM/disk.qcow2" 40G
    echo "$VM/disk.qcow2" > "$VM/cur"
    local t0 tr
    t0=$(now)
    start_vm "$VM/disk.qcow2" "" \
        -drive if=none,id=cd0,file="$VM/win.iso",media=cdrom,readonly=on \
        -device ide-cd,drive=cd0,bus=ide.1,bootindex=1 \
        -drive if=none,id=cd1,file="$VM/config.iso",media=cdrom,readonly=on \
        -device ide-cd,drive=cd1,bus=ide.2
    wait_ssh 3600 60 install 900
    say "TIMING install_to_first_command=$(( $(now) - t0 ))s"
    gssh 'powershell -NoProfile -Command "(Get-CimInstance Win32_OperatingSystem).Caption + \" \" + (Get-CimInstance Win32_OperatingSystem).Version"' | tr -d '\r' | sed 's/^/GUEST /'
    # The first logon sets the boot up; wait for it, bounded.
    for _ in $(seq 1 60); do
        grep -q 'firstlogon done' "$VM/serial.log" && break
        serial_new
        sleep 5
    done
    serial_new
    grep -q 'firstlogon done' "$VM/serial.log" || { echo "the first logon did not finish"; return 1; }
    # A clean restart before any cut: the spike found the boot policy the
    # first logon set gone after a cut 29 s after bcdedit; after a clean
    # restart it is on disk. Held to a new boot time: run 4's restart
    # answered SSH again 13 s after `shutdown /r`, where others took 137 s.
    local booted again=
    booted=$(boot_time)
    tr=$(now)
    gssh 'shutdown /r /t 0' >/dev/null 2>&1 || true
    for _ in $(seq 1 120); do
        again=$(boot_time || true)
        [ -n "$again" ] && [ "$again" != "$booted" ] && break
        sleep 5
    done
    wait_ssh 600 10 restart 240
    again=$(boot_time)
    if [ "$again" = "$booted" ]; then
        echo "the guest did not restart: it booted at $booted still"
        return 1
    fi
    say "TIMING clean_restart_to_first_command=$(( $(now) - tr ))s"
    gssh 'bcdedit /enum {default}' | tr -d '\r' | grep -E 'bootstatuspolicy|recoveryenabled' | sed 's/^/BCD /' || true
}

# What the file held when a server could not open it, into a log the run
# uploads: its length, and its last 64 KiB and the 4 KiB around the byte the
# error names ("... at <n>") as hex (dump.ps1). Run 42533808035 kept no file
# and said "unknown record kind 0" alone. Never fails the step itself.
file_dump() {
    local tag=$1
    local out=$VM/server-$tag-file.log at name
    at=$(tr -d '\r' < "$VM/server-$tag.log" | grep -oE ' at [0-9]+' | tail -1 | grep -oE '[0-9]+' || true)
    {
        gscp "$HERE/dump.ps1" Administrator@127.0.0.1:dump.ps1 &&
            gssh "powershell -NoProfile -ExecutionPolicy Bypass -File dump.ps1 ${at:+-At $at}" | tr -d '\r'
        for name in tail at; do
            gscp "Administrator@127.0.0.1:$name.bin" "$VM/$name.bin" 2>/dev/null || continue
            echo "== $name.bin"
            od -A d -t x1z "$VM/$name.bin"
            rm -f "$VM/$name.bin"
        done
    } > "$out" 2>&1 || true
    echo "FILE what the server found: $out"
}

# The server over a session of its own, its output the host's file: the
# session ends with QEMU. The token comes from the file the host copied in,
# never the command line (argv shows up in a process listing) nor the log.
serve() {
    local tag=$1 t0 code sp
    t0=$(now)
    setsid sshpass -p "$PW" ssh "${SSHO[@]}" -p 2222 Administrator@127.0.0.1 \
        'set /p FENNEC_HTTP_TOKEN=<C:\ci\token & C:\ci\fennec-server.exe --file C:\d\t.fennec --http 0.0.0.0:8080 --sync always' \
        > "$VM/server-$tag.log" 2>&1 < /dev/null &
    sp=$!
    for _ in $(seq 1 120); do
        if ! kill -0 "$sp" 2>/dev/null; then
            echo "SERVER $tag exited before it answered"
            tr -d '\r' < "$VM/server-$tag.log"
            file_dump "$tag"
            return 1
        fi
        code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 5 http://127.0.0.1:8080/_health || true)
        if [ "$code" = 200 ]; then
            say "SERVER $tag answered /_health $(( $(now) - t0 ))s after its start"
            tr -d '\r' < "$VM/server-$tag.log" | sed "s/^/SERVER $tag: /"
            grep -q '^opened: ' "$VM/server-$tag.log" || { echo "no \`opened:\` line"; return 1; }
            return 0
        fi
        sleep 1
    done
    echo "SERVER $tag: no answer from /_health in 120 s"
    tr -d '\r' < "$VM/server-$tag.log"
    return 1
}

fennec() {
    local t0 ok= zip
    t0=$(now)
    for _ in $(seq 1 120); do
        if gh run download "$GITHUB_RUN_ID" -R "$GITHUB_REPOSITORY" -n fennec-windows-zip -D "$VM/zip" >/dev/null 2>&1; then
            ok=1
            break
        fi
        if [ "$(gh run view "$GITHUB_RUN_ID" -R "$GITHUB_REPOSITORY" --json jobs \
                -q '.jobs[] | select(.name | startswith("fennec-server.exe")) | .conclusion' 2>/dev/null)" = failure ]; then
            echo "the release zip's build failed"
            return 1
        fi
        sleep 15
    done
    [ -n "$ok" ] || { echo "no release zip after $(( $(now) - t0 ))s"; return 1; }
    say "TIMING zip_wait=$(( $(now) - t0 ))s"
    zip=$(ls "$VM"/zip/*.zip)
    say "ZIP $(basename "$zip") $(wc -c < "$zip") bytes"
    gscp "$zip" Administrator@127.0.0.1:fennec.zip
    gscp "$VM/token" Administrator@127.0.0.1:token
    gssh 'powershell -NoProfile -Command "Expand-Archive -Force fennec.zip C:\ci\zip; Copy-Item (Get-ChildItem C:\ci\zip -Recurse -Filter fennec-server.exe).FullName C:\ci\fennec-server.exe; Move-Item -Force token C:\ci\token; New-Item -ItemType Directory -Force C:\d | Out-Null"'
    say "FENNEC $(gssh 'C:\ci\fennec-server.exe --version' | tr -d '\r')"
    # What the guest's disk says of its cache: a volatile one, enabled, that
    # Windows flushes -- what the lost cuts take away.
    gssh 'powershell -NoProfile -Command "Get-PhysicalDisk | Get-StorageAdvancedProperty | Format-List"' | tr -d '\r' |
        grep -E 'Cache|Power' | sed 's/^ */DISK /' | while read -r l; do say "$l"; done || true
    serve 0
    "${CLIENT[@]}" setup
    # The rig's own files on disk before any cut: run 4 lost the token,
    # copied in a minute before its first cut and never flushed, and the
    # server rightly refused to listen without one.
    gssh 'powershell -NoProfile -Command "Write-VolumeCache -DriveLetter C"'
}

# The server that opened the file after the cut holds its lock: a second
# one over the file is refused, naming it. Bounded: one that is not refused
# would serve.
lock_held() {
    local out
    out=$(timeout 60 sshpass -p "$PW" ssh "${SSHO[@]}" -p 2222 Administrator@127.0.0.1 \
        'C:\ci\fennec-server.exe --file C:\d\t.fennec --http 127.0.0.1:0' 2>&1 | tr -d '\r' || true)
    if ! grep -q 'it is open already' <<< "$out"; then
        echo "LOCK $1: a second server over the file was not refused: $out"
        return 1
    fi
    say "LOCK $1: a second server over the file refused: $out"
}

# The disk as of the last flush cut N's log holds: an overlay on the image
# the logged boot began from, the log replayed into it through qemu-nbd.
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
    # Gigabytes, and of no use once replayed.
    rm -f "$VM/log-$n.bin"
}

# Boot $3 (logging to $4 when given), the server on the same file, the
# check of cut $1 as $2, kept as what the next cut is held to with $5.
boot_check() {
    local n=$1 label=$2 disk=$3 log=$4 commit=${5:-} t0
    t0=$(now)
    start_vm "$disk" "$log"
    wait_ssh 600 10 "boot$n-$label" 240 60
    say "TIMING cut${n}_${label}_boot_to_first_command=$(( $(now) - t0 ))s"
    # What the cut left beside the file: its lock, and the image a compact
    # it stopped was writing (`.beside`, which the next compact writes
    # anew). Windows keeps no sync log (docs/design/windows.md, decision 3),
    # so nothing else.
    gssh 'dir /b C:\d' | tr -d '\r' | grep -v '^$' > "$VM/files-$n-$label.txt"
    say "FILES cut $n $label: $(tr '\n' ' ' < "$VM/files-$n-$label.txt")"
    if grep -v -x -E 't\.fennec|t\.fennec\.lock|t\.fennec\.beside' "$VM/files-$n-$label.txt"; then
        echo "a file the server does not keep is beside the database"
        return 1
    fi
    if ! gssh 'powershell -NoProfile -Command "if ((Get-Item C:\ci\token).Length -gt 0 -and (Test-Path C:\ci\fennec-server.exe)) { exit 0 } else { exit 1 }"'; then
        echo "the rig's own files did not survive the cut: the token or fennec-server.exe in C:\\ci"
        return 1
    fi
    serve "$n-$label"
    lock_held "$n-$label"
    t0=$(now)
    "${CLIENT[@]}" check --cut "$n" --label "$label" --cut-time "$(tr '\n' ' ' < "$VM/cut-$n.time")" $commit |
        tee "$VM/check-$n-$label.log"
    grep -E '^(VERDICT|DOCS|CHURN|SPILL|IDEM|LOAD|BATCH|INFLIGHT|PROBE)' "$VM/check-$n-$label.log" >> "$VM/summary.txt"
    say "TIMING cut${n}_${label}_check=$(( $(now) - t0 ))s"
}

power_cut() {
    local n=$1 mode=$2 next=$3 lp delay cur base log
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
    install | fennec | summary) "$1" ;;
    cut) shift; power_cut "$@" ;;
    *) echo "usage: vm.sh install | fennec | cut N plain|lost plain|log | summary" >&2; exit 2 ;;
esac
