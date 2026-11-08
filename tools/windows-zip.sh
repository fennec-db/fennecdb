#!/bin/sh
# The Windows release's zip, as release.yml makes it on a tag and CI's
# `windows (release zip)` job makes it on every change before one:
#
#   tools/windows-zip.sh VERSION TARGET DIST
#
# `fennec` on the cli profile and `fennec-server` on release, as every
# release builds them (docs/design/crates.md#profiles), for TARGET --
# x86_64-pc-windows-msvc, aarch64-pc-windows-msvc -- with the C runtime
# linked in (`+crt-static`): a fresh Windows has no Visual C++
# redistributable to lend them, and tools/windows-zip-check.ps1 holds them to
# importing none. Then DIST/fennec-vVERSION-TARGET.zip, holding the folder of
# that name the tarballs hold -- fennec.exe, fennec-server.exe, LICENSE,
# README.md, CONTRIBUTING.md. Run from the repository's root under the
# runner's bash (Git for Windows), as the tarballs' build runs under a Unix's;
# docs/design/windows.md, decision 6.

set -eu

if [ $# -ne 3 ]; then
    echo "usage: tools/windows-zip.sh VERSION TARGET DIST" >&2
    exit 2
fi
version=$1
target=$2
dist=$3
case $target in
    *-pc-windows-msvc) ;;
    *)
        echo "windows-zip: $target is not a Windows target" >&2
        exit 2
        ;;
esac

# With --target given, RUSTFLAGS reach the target's code alone, not a build
# script; ring's C is compiled /MT by them, cc reading the feature.
RUSTFLAGS="-C target-feature=+crt-static"
export RUSTFLAGS
cargo build --profile cli --target "$target" -p fennec-cli
cargo build --release --target "$target" -p fennec-server

stage=fennec-v$version-$target
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/$stage" "$dist"
cp "target/$target/cli/fennec.exe" "target/$target/release/fennec-server.exe" "$work/$stage/"
cp LICENSE README.md CONTRIBUTING.md "$work/$stage/"

# Windows' own tar (libarchive's bsdtar), which writes a zip by its name's
# suffix, and not Git's, which writes none.
windows=${SYSTEMROOT:-C:\\Windows}
bsdtar=$(cygpath -u "$windows")/System32/tar.exe
(cd "$work" && "$bsdtar" -a -c -f "$stage.zip" "$stage")
rm -f "$dist/$stage.zip"
mv "$work/$stage.zip" "$dist/$stage.zip"

echo "$stage.zip  $(wc -c <"$dist/$stage.zip" | tr -d ' ') bytes"
for b in fennec.exe fennec-server.exe; do
    echo "  $b  $(wc -c <"$work/$stage/$b" | tr -d ' ') bytes"
done
