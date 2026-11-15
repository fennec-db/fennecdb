#!/usr/bin/env bash
# tools/release-bins.sh's own refusal of a tampered tarball: a release made
# here -- a tarball per target holding two scripts that say their version,
# and its SHA256SUMS -- handed to it through OLD_ASSETS_DIR, once with a
# byte of the tarball's copy flipped and once as it is.
#
#   bash tools/test_release_bins.sh
#
# Flipped, install.sh refuses it by its sum and the script stops, nothing
# written to OUT_DIR. As it is, the sum holds ("Checksum ok") and the
# script goes on to the router, which this clone has no tag to build from:
# it stops there, OUT_DIR still empty -- so the first refusal was the
# flipped byte's, not something else's. Run by ci.yml's fmt job and by
# compat.yml before the matrix; needs python3, tar, gzip and a sha256 tool.

set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=$(mktemp -d "${TMPDIR:-/tmp}/fennec-test-release-bins.XXXXXX")
trap 'rm -rf "$WORK"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

fail() {
    echo "test_release_bins: FAILED: $*" >&2
    exit 1
}

TAG=v9.9.9-test
sum() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi
}
mkdir -p "$WORK/good" "$WORK/tampered" "$WORK/pack"
for target in aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-musl aarch64-unknown-linux-musl; do
    d=$WORK/pack/fennec-$TAG-$target
    mkdir -p "$d"
    for b in fennec-server fennec; do
        printf '#!/bin/sh\necho "%s %s"\n' "$b" "${TAG#v}" >"$d/$b"
        chmod 755 "$d/$b"
    done
    tar -czf "$WORK/good/fennec-$TAG-$target.tar.gz" -C "$WORK/pack" "fennec-$TAG-$target"
    echo "$(sum "$WORK/good/fennec-$TAG-$target.tar.gz")  fennec-$TAG-$target.tar.gz" >>"$WORK/good/SHA256SUMS"
done
cp "$WORK/good/"* "$WORK/tampered/"
for f in "$WORK/tampered/"*.tar.gz; do
    # One byte in the middle of the copy, xor 1: the sum the release
    # published no longer holds.
    python3 -I -c '
import sys
p = sys.argv[1]
b = bytearray(open(p, "rb").read())
b[len(b) // 2] ^= 1
open(p, "wb").write(bytes(b))
' "$f"
done

run() {
    local assets=$1 out=$2
    OLD_ASSETS_DIR=$assets bash "$ROOT/tools/release-bins.sh" "$TAG" "$out" >"$out.log" 2>&1
}

# Flipped: refused by its sum, nothing installed.
mkdir "$WORK/out-tampered"
if run "$WORK/tampered" "$WORK/out-tampered"; then
    fail "a tampered tarball was taken: $(tr '\n' ' ' <"$WORK/out-tampered.log")"
fi
grep -q 'the checksum of fennec-'"$TAG"'-.*SHA256SUMS says .*nothing was installed' "$WORK/out-tampered.log" ||
    fail "a tampered tarball was refused, but not by its sum: $(tr '\n' ' ' <"$WORK/out-tampered.log")"
grep -q 'nothing installed in' "$WORK/out-tampered.log" ||
    fail "release-bins did not say it installed nothing: $(tr '\n' ' ' <"$WORK/out-tampered.log")"
[ -z "$(ls -A "$WORK/out-tampered")" ] ||
    fail "a tampered tarball left files in OUT_DIR: $(ls -A "$WORK/out-tampered" | tr '\n' ' ')"

# As it is: the sum holds, and it stops at the router, nothing installed.
mkdir "$WORK/out-good"
if run "$WORK/good" "$WORK/out-good"; then
    fail "a release with no tag to build its router from was installed: $(tr '\n' ' ' <"$WORK/out-good.log")"
fi
grep -q 'Checksum ok' "$WORK/out-good.log" ||
    fail "the untampered tarball's sum did not hold: $(tr '\n' ' ' <"$WORK/out-good.log")"
grep -q "this clone has no tag $TAG" "$WORK/out-good.log" ||
    fail "the untampered release stopped elsewhere than at its router: $(tr '\n' ' ' <"$WORK/out-good.log")"
[ -z "$(ls -A "$WORK/out-good")" ] ||
    fail "a release stopped at its router left files in OUT_DIR: $(ls -A "$WORK/out-good" | tr '\n' ' ')"

echo "test_release_bins: ok: a tarball with a byte flipped refused by its sum, nothing installed; the same release untampered past its sum, stopped at its router, nothing installed"
