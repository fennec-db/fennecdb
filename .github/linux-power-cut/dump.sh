# What a database file held when its server could not open it after a cut,
# or its check failed: run in the Linux guest by vm.sh's file_dump as
# `bash -s -- [AT] [DIR] < dump.sh`, its output kept in the run's logs. The
# directory's listing, the file's length, its last 64 KiB and the 4 KiB
# around byte AT (the byte the server's error names) as hex, and the sync
# log's header: dump.ps1's counterpart. DIR is the copy taken before the
# server opened the file, /d itself where there is none.
set -u
at=${1:-}
dir=${2:-/d}
f=$dir/t.fennec
ls -la --time-style=full-iso "$dir"
[ -f "$f" ] || { echo "no $f"; exit 0; }
n=$(stat -c %s "$f")
echo "length $n"
from=$(( n > 65536 ? n - 65536 : 0 ))
echo "== the last 64 KiB, from byte $from"
od -A d -j "$from" -t x1z "$f"
if [ -n "$at" ]; then
    from=$(( at > 2048 ? at - 2048 : 0 ))
    echo "== the 4 KiB around byte $at, from byte $from"
    od -A d -j "$from" -N 4096 -t x1z "$f"
fi
if [ -f "$f.sync" ]; then
    echo "== the sync log's header and its first entry's head"
    od -A d -N 64 -t x1z "$f.sync"
    od -A d -j 4096 -N 32 -t x1z "$f.sync"
fi
