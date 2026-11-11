#!/usr/bin/env python3
"""What the sync log beside a database holds after a cut, read before the
server opens the file: whether its header is live and names the file, and
what of it the open will write back because the file lost it -- the path a
durable write takes on Linux (crates/fennec-core/src/synclog.rs), which a
cut tests only where the file lost what the log kept.

    synclog.py FILE LOG

Run in the Linux guest by vm.sh, on the image's python3; the standard
library alone, and it writes nothing. It reads the log as `naming` and
`recover` read it -- the header's magic, version and hash, the file's
device and inode, the fingerprint of the 4 KiB before the generation's
start, then each entry whole by its hash, from where the last one ended --
so a change to the log's format changes this file too.
"""

import os
import sys

M = (1 << 64) - 1
LOG_MAGIC = b"FENECSYN"
VERSION = 2
ENTRY_MAGIC = 0x4E595346
HEAD = 4096
HEADER = 64
ENTRY_HEAD = 32
PRINT = 4096


def rotl(x, r):
    return ((x << r) | (x >> (64 - r))) & M


def h64(seed, b):
    """synclog.rs's `hash`."""
    k = 0x9E3779B97F4A7C15
    h = (seed ^ (len(b) * 0xA0761D6478BD642F)) & M
    n = len(b) // 8
    for i in range(n):
        h = rotl(((h ^ int.from_bytes(b[8 * i : 8 * i + 8], "little")) * k) & M, 29)
    last = b[8 * n :] + bytes(8 - (len(b) - 8 * n))
    h = ((h ^ int.from_bytes(last, "little")) * k) & M
    h ^= h >> 33
    h = (h * 0xFF51AFD7ED558CCD) & M
    h ^= h >> 33
    h = (h * 0xC4CEB9FE1A85EC53) & M
    return h ^ (h >> 33)


def main():
    path, log_path = sys.argv[1], sys.argv[2]
    if not os.path.isfile(log_path):
        print("no log beside the file")
        return
    with open(log_path, "rb") as f:
        log = f.read()
    st = os.stat(path)
    size = st.st_size
    word = lambda at: int.from_bytes(log[at : at + 8], "little")
    if len(log) < HEADER or log[:8] != LOG_MAGIC:
        print(f"a log of {len(log)} bytes, its header wiped: spent by an open, or not begun yet; the file {size} bytes")
        return
    if int.from_bytes(log[8:12], "little") != VERSION or word(56) != h64(0, log[:56]):
        print(f"a log whose header is not whole: none applied; the file {size} bytes")
        return
    gen, base, printed, dev, ino = word(16), word(24), word(32), word(40), word(48)
    if (dev, ino) != (st.st_dev, st.st_ino):
        print(f"a live log of generation {gen} naming another file (device {dev}, inode {ino}): none applied")
        return
    if size < base:
        print(f"a live log of generation {gen} from byte {base}, and the file is {size} bytes: none applied")
        return
    with open(path, "rb") as f:
        frm = max(base - PRINT, 0)
        f.seek(frm)
        if h64(base, f.read(base - frm)) != printed:
            print(f"a live log of generation {gen} from byte {base}, and the file's bytes before it are not those it names: none applied")
            return
        at, pos = base, HEAD
        entries = differ = past = 0
        while pos + ENTRY_HEAD <= len(log):
            u32 = lambda i: int.from_bytes(log[pos + i : pos + i + 4], "little")
            u64 = lambda i: int.from_bytes(log[pos + i : pos + i + 8], "little")
            if u32(0) != ENTRY_MAGIC or u64(8) != gen or u64(16) != at:
                break
            n = u32(4)
            body = log[pos + ENTRY_HEAD : pos + ENTRY_HEAD + n]
            if len(body) < n or u64(24) != h64(gen ^ rotl(at, 17), body):
                break
            entries += 1
            f.seek(at)
            held = f.read(n)
            if held != body:
                differ += 1
                past += max(0, at + n - max(size, at))
            at += n
            pos += ENTRY_HEAD + n
    print(
        f"a live log of generation {gen} naming the file, from byte {base}: {entries} entries whole to byte {at}; "
        f"the file {size} bytes, {differ} of the entries not in it as written, {past} bytes of them past its end"
        + (": the open puts them back" if differ else ": nothing for the open to put back")
    )


if __name__ == "__main__":
    main()
