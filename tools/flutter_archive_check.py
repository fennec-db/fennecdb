#!/usr/bin/env python3
"""The Flutter plugin as pub.dev gets it: `python3 tools/flutter_archive_check.py
<archive.tar.gz>`, the archive `flutter pub publish --to-archive` writes of
the plugin prepare.sh filled, copied out of the repository as packages.yml
publishes it. CI's Flutter plugin job runs it before building apps from that
archive. A Markdown table of the archive and of each Mach-O in it goes to
stdout, for the step summary; each finding goes to stderr, and the exit is 1.

pub keeps no symbolic link: it packs what one points at. 0.2.0 went out with
the whole dynamic XCFramework under ios/ and again under macos/, its macOS
framework's binary three times in each (Versions/A, Versions/Current and the
top level): 34.5 MB packed, 65.7 MB unpacked, where each platform's library
once is under 25. Held here:

- the archive is at most LIMIT_PACKED bytes, and what it unpacks to at most
  LIMIT_UNPACKED: the libraries once, with room to grow by about the 5% the
  1.0 plan gives a native library. Raising one is a pull request that states
  the measured cost;
- no file of BIG bytes or more holds the same bytes as another, and no
  Mach-O slice is in two files;
- a Mach-O is under ios/ or macos/ alone, each of its slices built for that
  directory's platform -- the device's or the simulator's under ios/,
  macOS's under macos/ -- and for no newer a system than its podspec's
  `s.platform`, the lowest one CocoaPods lets an app target with the pod.
"""

import hashlib
import re
import struct
import sys
import tarfile

LIMIT_PACKED = 14_000_000
LIMIT_UNPACKED = 26_000_000
# A file this big held twice is a library shipped twice; the plugin's own
# small files may repeat.
BIG = 64 * 1024

MACOS, IOS, IOS_SIMULATOR = 1, 2, 7
PLATFORMS = {1: "macOS", 2: "iOS", 3: "tvOS", 4: "watchOS", 5: "bridgeOS", 6: "Mac Catalyst",
             7: "iOS simulator", 8: "tvOS simulator", 9: "watchOS simulator", 10: "DriverKit",
             11: "visionOS", 12: "visionOS simulator"}
# Each directory a Mach-O may be under: the platforms its slices may be
# built for, and the podspec whose `s.platform` bounds their minimum.
PLACES = {
    "ios/": ({IOS, IOS_SIMULATOR}, "ios/fennecdb_flutter.podspec"),
    "macos/": ({MACOS}, "macos/fennecdb_flutter.podspec"),
}
CPUS = {7: "i386", 0x01000007: "x86_64", 12: "arm", 0x0100000C: "arm64", 0x0200000C: "arm64_32"}

LC_BUILD_VERSION = 0x32
# The load commands before LC_BUILD_VERSION, by the platform each names.
LC_VERSION_MIN = {0x24: MACOS, 0x25: IOS}
PODSPEC_PLATFORM = re.compile(r"^\s*s\.platform\s*=\s*:(?:ios|osx)\s*,\s*'(\d+(?:\.\d+){0,2})'", re.M)


def slices(data):
    """A Mach-O's slices as (cpu type, bytes), or None for any other file."""
    if len(data) < 8:
        return None
    magic, count = struct.unpack_from(">II", data)
    if magic in (0xCAFEBABE, 0xCAFEBABF):
        # A Java class file starts with 0xCAFEBABE too, its version where a
        # fat header has its count of slices: 45 and up.
        if count == 0 or count >= 20:
            return None
        wide = magic == 0xCAFEBABF
        out = []
        for i in range(count):
            if wide:
                cpu, _, offset, size, _, _ = struct.unpack_from(">IIQQII", data, 8 + 32 * i)
            else:
                cpu, _, offset, size, _ = struct.unpack_from(">IIIII", data, 8 + 20 * i)
            out.append((cpu, data[offset:offset + size]))
        return out
    if struct.unpack_from("<I", data)[0] in (0xFEEDFACE, 0xFEEDFACF):
        return [(struct.unpack_from("<I", data, 4)[0], data)]
    return None


def build(thin):
    """A thin Mach-O's (platform, minimum), its minimum as the load command
    encodes it (X.Y.Z as xxxx.yy.zz); None where it names neither."""
    header = 32 if struct.unpack_from("<I", thin)[0] == 0xFEEDFACF else 28
    ncmds = struct.unpack_from("<I", thin, 16)[0]
    at = header
    for _ in range(ncmds):
        if at + 16 > len(thin):
            break
        cmd, size, first = struct.unpack_from("<III", thin, at)
        if cmd == LC_BUILD_VERSION:
            return first, struct.unpack_from("<I", thin, at + 12)[0]
        if cmd in LC_VERSION_MIN:
            return LC_VERSION_MIN[cmd], first
        if size == 0:
            break
        at += size
    return None


def version(v):
    """An encoded version as it is written: 11.0, 15.0, 10.12.2."""
    text = f"{v >> 16}.{(v >> 8) & 0xFF}"
    return text + (f".{v & 0xFF}" if v & 0xFF else "")


def encode(text):
    parts = [int(p) for p in text.split(".")] + [0, 0]
    return (parts[0] << 16) | (parts[1] << 8) | parts[2]


def megabytes(n):
    return f"{n / 1e6:.2f} MB ({n} bytes)"


def check(path):
    """The archive at path: the table's lines and the findings."""
    findings = []
    with open(path, "rb") as f:
        packed = len(f.read())
    files = {}
    with tarfile.open(path, "r:gz") as tar:
        for m in tar.getmembers():
            name = m.name[2:] if m.name.startswith("./") else m.name
            if m.issym() or m.islnk():
                findings.append(f"{name}: a link, which pub never packs -- is this pub's archive?")
            elif m.isfile():
                files[name] = tar.extractfile(m).read()
    unpacked = sum(len(b) for b in files.values())
    for what, n, limit in (("packed", packed, LIMIT_PACKED), ("unpacked", unpacked, LIMIT_UNPACKED)):
        if n > limit:
            findings.append(f"the archive is {megabytes(n)} {what}, past the {megabytes(limit)} it is held to")

    seen, copies = {}, set()
    for name in sorted(files):
        data = files[name]
        if len(data) >= BIG:
            digest = hashlib.sha256(data).hexdigest()
            if digest in seen:
                findings.append(f"{name} holds the same bytes as {seen[digest]}")
                copies.add(name)
            else:
                seen[digest] = name

    minimums = {}
    for place, (_, spec) in PLACES.items():
        found = PODSPEC_PLATFORM.findall(files.get(spec, b"").decode("utf-8", "replace"))
        if len(found) == 1:
            minimums[place] = encode(found[0])
        else:
            findings.append(f"{spec}: no single `s.platform = :ios|:osx, 'X.Y'` to hold {place}'s Mach-Os to")

    lines = ["| fennecdb_flutter, as pub.dev gets it | size | held to |", "| --- | ---: | ---: |",
             f"| packed | {megabytes(packed)} | {megabytes(LIMIT_PACKED)} |",
             f"| unpacked | {megabytes(unpacked)} | {megabytes(LIMIT_UNPACKED)} |", "",
             "| Mach-O | size | slices |", "| --- | ---: | --- |"]
    holders = {}
    for name in sorted(files):
        parts = slices(files[name])
        if parts is None:
            continue
        place = next((p for p in PLACES if name.startswith(p)), None)
        described = []
        for cpu, thin in parts:
            arch = CPUS.get(cpu, hex(cpu))
            digest = hashlib.sha256(thin).hexdigest()
            # A whole copy is named once, above, and not again by its slices.
            if digest in holders and holders[digest] != name and name not in copies:
                findings.append(f"{name}: its {arch} slice is in {holders[digest]} too")
            holders.setdefault(digest, name)
            named = build(thin) if len(thin) >= 32 else None
            if named is None:
                described.append(f"{arch}, no platform")
                findings.append(f"{name}: its {arch} slice names no platform (no LC_BUILD_VERSION)")
                continue
            platform, minimum = named
            described.append(f"{arch} {PLATFORMS.get(platform, platform)} {version(minimum)}")
            if place is None:
                continue
            allowed, spec = PLACES[place]
            if platform not in allowed:
                want = " and ".join(PLATFORMS[p] for p in sorted(allowed))
                findings.append(f"{name}: its {arch} slice is built for {PLATFORMS.get(platform, platform)}; "
                                f"{place} holds {want} alone")
            elif place in minimums and minimum > minimums[place]:
                findings.append(f"{name}: its {arch} slice asks for {PLATFORMS[platform]} {version(minimum)}, "
                                f"newer than the {version(minimums[place])} {spec} declares")
        if place is None:
            findings.append(f"{name}: a Mach-O outside ios/ and macos/")
        lines.append(f"| `{name}` | {megabytes(len(files[name]))} | {'; '.join(described)} |")
    return lines, findings


def main(argv):
    if len(argv) != 2:
        sys.exit("usage: flutter_archive_check.py <fennecdb_flutter.tar.gz>")
    lines, findings = check(argv[1])
    print("\n".join(lines))
    if findings:
        sys.exit("\n".join(findings))


if __name__ == "__main__":
    main(sys.argv)
