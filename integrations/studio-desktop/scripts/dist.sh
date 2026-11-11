#!/bin/sh
# The app as it is downloaded: the stable build unpacked, signed ad hoc as a
# whole, and put in a DMG beside a link to /Applications.
#
#   scripts/dist.sh [build .app] [out .dmg]
#     default: build/stable-macos-arm64/fennec studio.app
#              artifacts/dist/fennec-studio-macos-arm64.dmg
#
# On Windows and Linux, the app unpacked in a zip or a tarball instead (below):
#
#   scripts/dist.sh
#     artifacts/dist/fennec-studio-windows-<x64|arm64>.zip   (Git's bash)
#     artifacts/dist/fennec-studio-linux-<x64|arm64>.tar.gz
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

# Windows and Linux: the folder Electrobun's installer would leave, and no
# installer. A stable build there is an installer, not an app (the build
# folder's launcher is the extractor, and the payload a .tar.zst in its
# Resources): on Windows `<name>-Setup.exe`, which on the runners said it had
# installed the app and then neither started it nor ended, and which a
# browser's download meets SmartScreen with; on Linux `installer`, which
# installs over a running copy and starts the app itself. Both also keep the
# whole payload again as a tar for an updater the app does not have (123 MB
# beside a 118 MB app on Linux). So the download is the payload unpacked --
# the files the installer copies into <appdata>/dev.fennecdb.studio/stable/app
# -- as one folder, fennec-studio, which runs where it is unpacked, and which
# install.ps1 -App and install.sh --app put where the installer would
# (checks/dist.mjs holds both). A zip on Windows, which Expand-Archive and
# Explorer open, written by Windows' own tar.exe (libarchive's bsdtar: Git's
# writes none), as tools/windows-zip.sh writes the programs'; a gzip tarball
# on Linux, which every system there opens, its modes kept.
archive() {
  case $(uname -s) in
    Linux) sys=linux ;;
    *) sys=windows ;;
  esac
  # The build Hutch made on this machine, named for its processor: on
  # windows-11-arm Git's bash runs emulated and `uname -m` says x86_64.
  set -- build/stable-*-*/*/Resources/*.tar.zst
  [ "$#" -eq 1 ] && [ -f "$1" ] || die "build/ holds no single stable build's payload (Resources/*.tar.zst): build it first (npm run build)"
  payload=$1
  dir=${payload#build/stable-}
  dir=${dir%%/*}
  case $dir in
    linux-x64 | linux-arm64 | win-x64 | win-arm64) ;;
    *) die "build/stable-$dir is not a Windows or Linux build" ;;
  esac
  arch=${dir#*-}
  case $dir in
    linux-*) [ "$sys" = linux ] || die "a Linux build, and this is not Linux" ;;
    *) [ "$sys" = windows ] || die "a Windows build, and this is not Windows" ;;
  esac
  name=fennec-studio-$sys-$arch
  if [ "$sys" = windows ]; then ext=zip; else ext=tar.gz; fi
  out=artifacts/dist/$name.$ext
  work=build/dist
  rm -rf "$work"
  mkdir -p "$work/unpacked" "$(dirname "$out")"

  # zstd through Node's own zlib (22.15 and later): the same on both systems,
  # where a zstd program may or may not be on the PATH.
  node -e 'const z = require("node:zlib"), f = require("node:fs"); f.writeFileSync(process.argv[2], z.zstdDecompressSync(f.readFileSync(process.argv[1])))' "$payload" "$work/payload.tar" ||
    die "could not decompress $payload"
  if [ "$sys" = windows ]; then
    tar=$(cygpath -u "${SYSTEMROOT:-C:\\Windows}")/System32/tar.exe
  else
    tar=tar
  fi
  (cd "$work/unpacked" && "$tar" -xf ../payload.tar) || die "could not unpack $payload"
  rm -f "$work/payload.tar"
  set -- "$work"/unpacked/*
  [ "$#" -eq 1 ] && [ -d "$1" ] || die "the payload holds more than the app's folder: $*"
  mv "$1" "$work/unpacked/fennec-studio"
  app=$work/unpacked/fennec-studio
  if [ "$sys" = windows ]; then launcher=$app/bin/launcher.exe; else launcher=$app/bin/launcher; fi
  [ -f "$launcher" ] || die "the payload holds no launcher ($launcher)"
  [ -f "$app/Resources/app/bun/index.js" ] || die "the payload holds no app (Resources/app/bun/index.js)"
  set -- "$app"/Resources/*.tar.zst
  [ ! -e "$1" ] || die "the unpacked app holds a payload of its own"

  rm -f "$out"
  if [ "$sys" = windows ]; then
    # Deflate at its best where this tar.exe's libarchive takes the option.
    (cd "$work/unpacked" && { "$tar" -a -c --options zip:compression-level=9 -f "../$name.zip" fennec-studio ||
      { rm -f "../$name.zip" && "$tar" -a -c -f "../$name.zip" fennec-studio; }; }) || die "could not write the zip"
  else
    (cd "$work/unpacked" && tar -c --owner=0 --group=0 --numeric-owner fennec-studio | gzip -9 >"../$name.tar.gz") || die "could not write the tarball"
  fi
  mv "$work/$name.$ext" "$out"
  echo "dist: $out, the app unpacked from $(basename "$payload"): $(wc -c <"$out" | tr -d ' ') bytes, $(du -sk "$app" | cut -f1) KB unpacked"
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$out"; fi
}

case $(uname -s) in
  Linux | MINGW* | MSYS* | CYGWIN*)
    archive
    exit 0
    ;;
esac

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

# A volume a run that stopped half-way left attached goes before its folder,
# and this run's goes whatever happens.
detach_volume() {
  if [ -d "$work/rw" ]; then hdiutil detach -force "$work/rw" >/dev/null 2>&1 || true; fi
}
detach_volume
trap detach_volume EXIT
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

# The volume: the app and a link to /Applications to drag it onto, written
# into an empty read-write image this script attaches itself, then
# compressed with LZMA (ULMO, macOS 10.15 and later; Apple silicon starts at
# 11), the smallest hdiutil makes (docs/design/integrations.md has the
# sizes). Not `hdiutil create -srcfolder`, which does the same with a detach
# of its own: on a hosted runner it failed once in two runs, after the copy
# (the classic "Resource busy": a scanner, XProtect or Spotlight, holding a
# file just written, here ten Mach-O files where Electrobun's own DMG has a
# tarball). Spotlight is told to leave the volume alone, everything is
# synced before the detach, and a detach still refused is forced: nothing is
# left to write, and the image is verified and held by checks/dist.mjs file
# by file. Not -quiet anywhere: it closes hdiutil's stderr too.
size=$(du -sm "$app" | cut -f1)
hdiutil create -megabytes $((size + size / 5 + 16)) -fs HFS+ -volname "fennec studio" -type UDIF -ov "$work/rw.dmg"
mkdir -p "$work/rw"
hdiutil attach -nobrowse -noautoopen -noverify -mountpoint "$work/rw" "$work/rw.dmg"
touch "$work/rw/.metadata_never_index"
ditto "$app" "$work/rw/$name"
ln -s /Applications "$work/rw/Applications"
sync
if ! hdiutil detach "$work/rw"; then
  echo "dist: the volume is held (above); everything is written and synced, so it is detached by force"
  hdiutil detach -force "$work/rw"
fi
rm -f "$out"
hdiutil convert "$work/rw.dmg" -format ULMO -o "$out"
hdiutil verify "$out"
ls -l "$out"
if command -v shasum >/dev/null 2>&1; then shasum -a 256 "$out"; fi
