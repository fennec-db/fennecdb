#!/bin/sh
# The app as it is downloaded: the stable build unpacked, signed ad hoc as a
# whole, and put in a DMG beside a link to /Applications.
#
#   scripts/dist.sh [build .app] [out .dmg]
#     default: build/stable-macos-arm64/fennec studio.app
#              artifacts/dist/fennec-studio-macos-arm64.dmg
#
# Unpacked before it ships. Electrobun's stable build is a self-extractor: a
# launcher (the extractor, linker-signed ad hoc) and the app as a .tar.zst in
# its Resources, which the first launch unpacks into the bundle itself. That
# cannot be shipped unsigned. Its signature does not hold (`codesign --verify
# --deep --strict`: "code has no resources but signature indicates they must
# be present"), and macOS refuses a quarantined download whose signature does
# not hold as "damaged", with no Open Anyway; and a quarantined app opened
# where Finder did not move it runs translocated, from a read-only mount,
# where an extractor cannot write into its bundle. So the bundle shipped is
# the one the first launch would leave -- the same payload untarred, file for
# file what `checks/warmup.mjs` leaves (docs/design/integrations.md) -- and
# nothing writes into it when it runs (`checks/dist.mjs` runs it from the
# DMG's own read-only mount).
#
# Signed ad hoc, inside out. Apple silicon runs no code without a signature,
# and Gatekeeper asks about a download whose signature holds once (Open
# Anyway) where it calls one that does not damaged. Ad hoc names no one: there
# is no Developer ID here (desktop.yml's signing job is for when there is).
# Each Mach-O file is signed first, the deepest first, then the bundle, which
# seals every other file; `--deep` is not used to sign, since it signs nested
# code with the outer bundle's options. No hardened runtime: Bun's JIT would
# need entitlements for it, and ad hoc code gets nothing from it.
set -eu
cd "$(dirname "$0")/.."

die() {
  echo "dist: $*" >&2
  exit 1
}

src=${1:-build/stable-macos-arm64/fennec studio.app}
out=${2:-artifacts/dist/fennec-studio-macos-arm64.dmg}
name=$(basename "$src")
work=build/dist
[ -d "$src" ] || die "no build at $src: make studio-desktop first"
for tool in tar codesign hdiutil ditto file xattr; do
  command -v "$tool" >/dev/null 2>&1 || die "needs $tool (macOS)"
done

# Exactly one payload: a build that is unpacked already (launched once) or
# holds two is not one this knows to have come from the build.
set -- "$src"/Contents/Resources/*.tar.zst
[ "$#" -eq 1 ] && [ -f "$1" ] || die "$src holds no single .tar.zst payload: is it a stable build that was never launched?"
payload=$1

rm -rf "$work"
mkdir -p "$work/unpacked" "$work/volume" "$(dirname "$out")"
tar -xf "$payload" -C "$work/unpacked" || die "could not unpack $payload"
app="$work/unpacked/$name"
[ -f "$app/Contents/Info.plist" ] || die "the payload holds no $name"
[ -f "$app/Contents/Resources/app/bun/index.js" ] || die "the payload's $name holds no app (Contents/Resources/app/bun/index.js)"
set -- "$app"/Contents/Resources/*.tar.zst
[ ! -e "$1" ] || die "the unpacked $name holds a payload of its own"
main=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$app/Contents/Info.plist")
[ -f "$app/Contents/MacOS/$main" ] || die "$name names $main as its executable and has none"

# codesign refuses a bundle that carries Finder information or a resource fork.
xattr -cr "$app"

# Every Mach-O file but the main executable, the deepest first; the bundle's
# own signature then covers the main executable and seals the rest.
find "$app" -type f | awk -F/ '{ print NF "\t" $0 }' | sort -rn | cut -f2- >"$work/files"
signed=0
while IFS= read -r f; do
  [ "$f" != "$app/Contents/MacOS/$main" ] || continue
  case $(file -b "$f") in
    Mach-O*)
      codesign --force --sign - --timestamp=none "$f"
      signed=$((signed + 1))
      ;;
  esac
done <"$work/files"
codesign --force --sign - --timestamp=none "$app"
codesign --verify --deep --strict --verbose=2 "$app"
echo "dist: $name unpacked from $(basename "$payload"), $signed nested Mach-O files and the bundle signed ad hoc"

# The volume: the app and a link to /Applications to drag it onto. Compressed
# with LZMA (ULMO, macOS 10.15 and later; Apple silicon starts at 11), the
# smallest hdiutil makes (docs/design/integrations.md has the sizes).
ditto "$app" "$work/volume/$name"
ln -s /Applications "$work/volume/Applications"
rm -f "$out"
# Not -quiet: it closes hdiutil's stderr too, and a failure says nothing.
hdiutil create -volname "fennec studio" -srcfolder "$work/volume" -fs HFS+ -format ULMO -ov "$out"
hdiutil verify "$out"
ls -l "$out"
if command -v shasum >/dev/null 2>&1; then shasum -a 256 "$out"; fi
