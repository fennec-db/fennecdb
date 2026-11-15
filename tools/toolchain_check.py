#!/usr/bin/env python3
"""The compiler CI tests with is the one that ships: `python3
tools/toolchain_check.py`, CI's fmt job.

rust-toolchain.toml names an exact release, X.Y.Z -- never `stable`, which
moved under main's CI with no change of ours (1.100.0's Unicode tables
failed fennec-core's case tests on every pull request the day it came out),
nor X.Y, which follows its newest patch. The images build with that same
release:

- every Dockerfile's rust image is `rust:X.Y.Z-...` of the file's release,
  by digest, and none of them removes rust-toolchain.toml from the build
  or sets RUSTUP_TOOLCHAIN, either of which builds with the image's own
  compiler whatever the file says -- the repository's Dockerfile removed it
  until the pin, when the channel was `stable` and no pin was lost;
- every FROM of the two images whose binaries ship or run the regions'
  checks (PINNED) names a digest -- but `scratch`, which is no image, and
  a stage of the file's own -- so a tag moved under it changes no build.

    python3 tools/toolchain_check.py            # exit 1, naming each difference
    python3 tools/toolchain_check.py --release  # the release, for scripts

A bump changes rust-toolchain.toml and every rust image's tag and digest in
one pull request (docker buildx imagetools inspect rust:X.Y.Z-alpine)."""

import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent

# The images whose every FROM names a digest: fennec-server's, which ships,
# and deploy/'s, the regions' kit its checks run.
PINNED = ("Dockerfile", "deploy/Dockerfile")

EXACT = re.compile(r"\d+\.\d+\.\d+")
CHANNEL = re.compile(r'^\s*channel\s*=\s*"([^"]*)"\s*(?:#.*)?$', re.M)
DIGEST = re.compile(r"@sha256:[0-9a-f]{64}")
FROM = re.compile(r"^\s*FROM\s+(?:--platform=\S+\s+)?(\S+)(?:\s+AS\s+(\S+))?\s*(?:#.*)?$", re.I)
# Docker Hub's official image under each name a FROM may give it.
RUST = {"rust", "library/rust", "docker.io/library/rust", "index.docker.io/library/rust"}


def release(text):
    """rust-toolchain.toml's release and None, or None and why it names none."""
    found = CHANNEL.findall(text)
    if len(found) != 1:
        return None, "rust-toolchain.toml names no single `channel`"
    name = found[0]
    if not EXACT.fullmatch(name):
        return None, (f"rust-toolchain.toml's channel is {name!r}: an exact release, X.Y.Z, "
                      "is what CI tests and the images ship")
    return name, None


def image_of(ref):
    """An image reference's name, its tag and whether it names a digest."""
    digest = bool(DIGEST.search(ref))
    bare = ref.split("@", 1)[0]
    name, colon, tag = bare.rpartition(":")
    # A colon before the last slash is a registry's port, not a tag.
    if not colon or "/" in tag:
        name, tag = bare, ""
    return name, tag, digest


def problems(path, text, want, pinned):
    """What a Dockerfile `path` holding `text` does against release `want`;
    `pinned`, whether its every FROM must name a digest."""
    out = []
    stages = set()
    rust = False
    for n, line in enumerate(text.splitlines(), 1):
        m = FROM.match(line)
        if not m:
            continue
        ref, stage = m.group(1), m.group(2)
        name, tag, digest = image_of(ref)
        if name.lower() in RUST:
            rust = True
            version = tag.split("-", 1)[0]
            if version != want or not digest:
                out.append(f"{path}:{n}: FROM {ref}: rust-toolchain.toml names {want}, so the "
                           f"image is rust:{want}-...@sha256:<digest> (docker buildx imagetools "
                           f"inspect rust:{want}-alpine)")
        elif pinned and ref != "scratch" and ref.lower() not in stages and not digest:
            out.append(f"{path}:{n}: FROM {ref} names no digest: pin it as "
                       f"{ref}@sha256:<digest> (docker buildx imagetools inspect {ref})")
        if stage:
            stages.add(stage.lower())
    if rust:
        # An instruction is a line and those its backslashes continue; a
        # comment is a whole line.
        code = [l for l in text.replace("\\\n", " ").splitlines() if not l.lstrip().startswith("#")]
        if any("RUSTUP_TOOLCHAIN" in l for l in code):
            out.append(f"{path}: sets RUSTUP_TOOLCHAIN, which builds with another compiler "
                       "than rust-toolchain.toml's")
        if any(re.search(r"^\s*RUN\b.*\brm\b.*rust-toolchain", l, re.I) for l in code):
            out.append(f"{path}: removes rust-toolchain.toml, which builds with the image's "
                       "own compiler whatever the file names")
    return out


def dockerfiles():
    """Every Dockerfile in the repository: git's list, or PINNED without one."""
    try:
        listed = subprocess.run(["git", "-C", str(ROOT), "ls-files", "-z"], check=True,
                                capture_output=True, text=True).stdout.split("\0")
    except (OSError, subprocess.CalledProcessError):
        return list(PINNED)
    names = [p for p in listed if p and re.fullmatch(
        r"(?:.*\.)?(?:Dockerfile|Containerfile)(?:\..*)?", p.rsplit("/", 1)[-1])]
    return sorted(set(names) | set(PINNED))


def main(argv):
    want, why = release((ROOT / "rust-toolchain.toml").read_text())
    if argv[1:] == ["--release"]:
        if not want:
            sys.exit(why)
        print(want)
        return
    if argv[1:]:
        sys.exit("usage: toolchain_check.py [--release]")
    if not want:
        sys.exit(why)
    found = []
    for path in dockerfiles():
        text = (ROOT / path).read_text()
        found += problems(path, text, want, path in PINNED)
    if found:
        sys.exit("\n".join(found))
    print(f"ok  rust-toolchain.toml: {want}, and every image's rust {want}, by digest")


if __name__ == "__main__":
    main(sys.argv)
