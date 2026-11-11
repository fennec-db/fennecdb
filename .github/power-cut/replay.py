#!/usr/bin/env python3
"""A disk whose write cache was lost: the writes QEMU's blklogwrites logged,
replayed up to the last flush the guest asked for.

    replay.py LOG SOCKET [--upto flush|all]

LOG is the log blklogwrites kept beside the guest's disk from its start
(dm-log-writes' format: a superblock, then an entry a sector, each followed
by the data it wrote). SOCKET is qemu-nbd's, serving an overlay on the disk
as it was when the log began. The writes up to the last flush entry are
written to it, in the order the log holds them -- the order their writes
completed, each entry allocated once its write is done
(block/blklogwrites.c) -- and those after it are dropped: what a disk that
honours flushes and loses its volatile cache at a power cut may come back
with, the worst of it. A write the guest saw complete before it asked for a
flush is before that flush in the log; a flush the guest saw complete is in
the log whole. The standard library alone.

The walk stops at the first entry that is not whole -- flags outside the
format's, or a header of zeros: the log's tail, still being written when
QEMU was killed. Discards are dropped: QEMU passes none down here (the
drive's `discard` is `ignore`), and a discarded range may read back as it
was.
"""

import socket
import struct
import sys

MAGIC = 0x6A736677736872
# How far short of its superblock's count a log may break off at a kill:
# an entry's place is taken once its write to the disk completes and its
# own write to the log follows, so only those still in flight -- a disk
# queue's depth, AHCI's 32 and virtio-blk's 256 per queue, of which the
# Linux guest's -smp 4 may run 4 (1 024 in all) -- can be missing at the tail.
# A log that breaks off further back lost an entry QEMU failed to log
# (run 43010736756: its guest saw EIO, and the walk stopped at 4 531 of
# 34 581), and replaying what is before the hole would rebuild a disk older
# than its last flush: an acknowledged write gone, the rig's doing.
IN_FLIGHT = 1024
FLUSH, FUA, DISCARD, MARK = 1, 2, 4, 8

NBD_REQUEST = 0x25609513
NBD_REPLY = 0x67446698
NBD_CMD_WRITE, NBD_CMD_DISC, NBD_CMD_FLUSH = 1, 2, 3


def entries(path):
    """The log's superblock and its whole entries, as (index, sector of the
    entry, disk sector, sectors, flags)."""
    with open(path, "rb") as f:
        sb = f.read(512)
        if len(sb) < 28:
            return None, []
        magic, version, nr, size = struct.unpack("<QQQI", sb[:28])
        if magic != MAGIC:
            # No flush ever came: the superblock is written at the first.
            return None, []
        if version != 1 or size != 512:
            raise SystemExit(f"a log of version {version}, sectors of {size}: not this script's")
        out = []
        at = 1
        while True:
            f.seek(at * size)
            h = f.read(32)
            if len(h) < 32:
                break
            sector, n, flags, _ = struct.unpack("<QQQQ", h)
            if flags & ~(FLUSH | FUA | DISCARD | MARK) or (flags == 0 and n == 0):
                break
            out.append((len(out), at, sector, n, flags))
            at += 1 + (0 if flags & DISCARD else n)
        return nr, out


class Nbd:
    """qemu-nbd's newstyle handshake and simple replies: enough to write."""

    def __init__(self, path):
        self.s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.s.connect(path)
        hello = self.recv(18)
        if hello[:8] != b"NBDMAGIC" or hello[8:16] != b"IHAVEOPT":
            raise SystemExit(f"not a newstyle NBD server: {hello!r}")
        (hflags,) = struct.unpack(">H", hello[16:18])
        no_zeroes = hflags & 2
        self.s.sendall(struct.pack(">I", 1 | no_zeroes))
        # NBD_OPT_EXPORT_NAME, qemu-nbd's default export: the empty name.
        self.s.sendall(b"IHAVEOPT" + struct.pack(">II", 1, 0))
        reply = self.recv(10 if no_zeroes else 134)
        self.size, _ = struct.unpack(">QH", reply[:10])
        self.handle = 0

    def recv(self, n):
        b = b""
        while len(b) < n:
            c = self.s.recv(n - len(b))
            if not c:
                raise SystemExit("the NBD server closed the connection")
            b += c
        return b

    def command(self, kind, offset=0, data=b""):
        self.handle += 1
        self.s.sendall(struct.pack(">IHHQQI", NBD_REQUEST, 0, kind, self.handle, offset, len(data)) + data)
        magic, error, handle = struct.unpack(">IIQ", self.recv(16))
        if magic != NBD_REPLY or handle != self.handle or error:
            raise SystemExit(f"NBD command {kind} at {offset} failed: error {error}")

    def close(self):
        self.command(NBD_CMD_FLUSH)
        self.s.sendall(struct.pack(">IHHQQI", NBD_REQUEST, 0, NBD_CMD_DISC, 0, 0, 0))
        self.s.close()


def main():
    log, sock = sys.argv[1], sys.argv[2]
    upto = sys.argv[3] if len(sys.argv) > 3 else "flush"
    nr, es = entries(log)
    if nr is not None and nr > len(es) + IN_FLIGHT:
        raise SystemExit(
            f"REPLAY the log breaks off at entry {len(es)}, and its superblock counted {nr}: "
            "a write the log did not take, the rig's fault and not the server's; nothing replayed"
        )
    flushes = [e for e in es if e[4] & FLUSH]
    if upto == "all":
        end = len(es)
    else:
        end = flushes[-1][0] + 1 if flushes else 0
    kept = [e for e in es[:end] if not e[4] & DISCARD and e[3]]
    dropped = [e for e in es[end:] if not e[4] & DISCARD and e[3]]
    discards = sum(1 for e in es if e[4] & DISCARD)
    mb = lambda xs: sum(e[3] for e in xs) * 512 / 1e6
    print(
        f"REPLAY {len(es)} entries whole in the log (its superblock counted {nr}), {len(flushes)} flushes; "
        f"{len(kept)} writes ({mb(kept):.1f} MB) up to the last flush replayed, "
        f"{len(dropped)} writes ({mb(dropped):.1f} MB) after it dropped, {discards} discards"
    )
    nbd = Nbd(sock)
    with open(log, "rb") as f:
        for _, at, sector, n, _ in kept:
            f.seek((at + 1) * 512)
            left, offset = n * 512, sector * 512
            while left:
                chunk = f.read(min(left, 8 << 20))
                if not chunk:
                    raise SystemExit(f"the log ends inside the entry at sector {at}")
                nbd.command(NBD_CMD_WRITE, offset, chunk)
                offset += len(chunk)
                left -= len(chunk)
    nbd.close()
    print("REPLAY done")


if __name__ == "__main__":
    main()
