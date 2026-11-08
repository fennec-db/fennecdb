#!/usr/bin/env python3
"""A copy of a Mach-O executable under another identity: its `LC_UUID` set,
then signed again ad hoc (`codesign -s - -f`).

    setuuid.py SRC DST [UUID]    DST a copy of SRC, its UUID set to UUID
                                 (32 hex digits; a random one when left out)
    setuuid.py --show BIN        BIN's UUID, one a slice of a universal binary

Why a copy under another UUID is worth running: macOS 26's allocator (xzone
malloc) takes the paths a binary's allocations and frees go down by the
binary's identity, so two builds of the same code, or one build with its
UUID alone rewritten, run one request of `replays.rs` some 500 to 1 100
instructions apart, every one of them the allocator's
(docs/design/benchmarks.md#instruction-counts). Each copy is another draw
of those paths; `compare.py` runs several of each build and averages them.

Only a copy is changed, never the build's own output: DST may not be SRC,
nor inside a cargo profile directory (one holding `.fingerprint`), nor an
existing file unless `--force`. The signing is macOS's (`codesign`);
elsewhere this says so and exits 0.
"""

import argparse
import os
import shutil
import struct
import subprocess
import sys
import uuid

MH_MAGIC_64 = 0xFEEDFACF
FAT_MAGIC = 0xCAFEBABE
FAT_MAGIC_64 = 0xCAFEBABF
LC_UUID = 0x1B


def slices(data):
    """The offsets of each thin 64-bit Mach-O image in `data`: one for a
    thin binary, one an architecture for a universal one."""
    (magic,) = struct.unpack_from(">I", data, 0)
    if magic in (FAT_MAGIC, FAT_MAGIC_64):
        (n,) = struct.unpack_from(">I", data, 4)
        offs = []
        for i in range(n):
            if magic == FAT_MAGIC:
                _, _, off, _, _ = struct.unpack_from(">iiIII", data, 8 + 20 * i)
            else:
                _, _, off, _, _, _ = struct.unpack_from(">iiQQII", data, 8 + 32 * i)
            offs.append(off)
        return offs
    (magic,) = struct.unpack_from("<I", data, 0)
    if magic != MH_MAGIC_64:
        raise ValueError(f"not a 64-bit Mach-O image (magic {magic:#x})")
    return [0]


def uuid_offset(data, base):
    """Where the 16 bytes of the `LC_UUID` of the image at `base` are."""
    (magic,) = struct.unpack_from("<I", data, base)
    if magic != MH_MAGIC_64:
        raise ValueError(f"not a 64-bit Mach-O image at {base} (magic {magic:#x})")
    ncmds, sizeofcmds = struct.unpack_from("<II", data, base + 16)
    off, end = base + 32, base + 32 + sizeofcmds
    for _ in range(ncmds):
        cmd, size = struct.unpack_from("<II", data, off)
        if cmd == LC_UUID:
            return off + 8
        if size < 8 or off + size > end:
            break
        off += size
    raise ValueError("no LC_UUID load command")


def read_uuids(path):
    with open(path, "rb") as f:
        data = f.read()
    return [str(uuid.UUID(bytes=data[o:o + 16])) for o in (uuid_offset(data, b) for b in slices(data))]


def build_output(path):
    """The cargo profile directory (`target/release`, ...) `path` is inside,
    told by its `.fingerprint`, or None."""
    d = os.path.dirname(os.path.realpath(path))
    while True:
        if os.path.isdir(os.path.join(d, ".fingerprint")):
            return d
        up = os.path.dirname(d)
        if up == d:
            return None
        d = up


def signs():
    """Whether this machine can sign a copy: macOS's `codesign`."""
    return sys.platform == "darwin" and shutil.which("codesign") is not None


def set_uuid(src, dst, want=None, sign=True, force=False):
    """Writes DST, a copy of SRC with its UUID set to `want` (hex; random
    when None) in every slice, and signs it again ad hoc; returns the UUID.
    A test passes `sign=False`: the copy then cannot run on Apple silicon."""
    if os.path.exists(dst) and os.path.samefile(src, dst):
        raise ValueError(f"{dst} is {src}: only a copy is changed")
    out = build_output(dst)
    if out:
        raise ValueError(f"{dst} is inside cargo's build output ({out}): a copy goes to a scratch directory")
    if os.path.exists(dst) and not force:
        raise ValueError(f"{dst} exists (--force writes over it)")
    if sign and not signs():
        raise RuntimeError("signing a copy needs macOS's codesign")
    new = uuid.UUID(hex=want) if want else uuid.uuid4()
    with open(src, "rb") as f:
        data = bytearray(f.read())
    for base in slices(data):
        o = uuid_offset(data, base)
        data[o:o + 16] = new.bytes
    tmp = dst + ".tmp"
    with open(tmp, "wb") as f:
        f.write(data)
    shutil.copymode(src, tmp)
    if sign:
        p = subprocess.run(["codesign", "-s", "-", "-f", tmp], capture_output=True, text=True)
        if p.returncode != 0:
            os.remove(tmp)
            raise RuntimeError(f"codesign {tmp}: {p.stderr.strip()}")
    os.replace(tmp, dst)
    return str(new)


def identities(src, n, directory, name=None):
    """`n` copies of SRC in `directory`, each under a random UUID: a list of
    (path, uuid)."""
    os.makedirs(directory, exist_ok=True)
    name = name or os.path.basename(src)
    out = []
    for i in range(n):
        dst = os.path.join(directory, f"{name}-id{i}")
        out.append((dst, set_uuid(src, dst, force=True)))
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--show", metavar="BIN", help="print BIN's UUID")
    ap.add_argument("--force", action="store_true", help="write over an existing DST")
    ap.add_argument("src", nargs="?")
    ap.add_argument("dst", nargs="?")
    ap.add_argument("uuid", nargs="?", help="32 hex digits; random when left out")
    a = ap.parse_args()
    try:
        if a.show:
            for u in read_uuids(a.show):
                print(u)
            return
        if not a.src or not a.dst:
            ap.error("SRC and DST, or --show BIN")
        if not signs():
            print("setuuid.py: a copy is signed by macOS's codesign; nothing to do on "
                  f"{sys.platform} (the allocator this averages is macOS's)", file=sys.stderr)
            return
        print(set_uuid(a.src, a.dst, a.uuid, force=a.force))
    except (ValueError, RuntimeError, OSError) as e:
        sys.exit(f"setuuid.py: {e}")


if __name__ == "__main__":
    main()
