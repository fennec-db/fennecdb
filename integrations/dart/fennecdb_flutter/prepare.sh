#!/bin/sh
# Puts the native library where each platform's project takes it from, each
# platform's alone:
#
#   ios/Frameworks/FennecFFI.xcframework  its iOS slices: FennecFFI.framework
#                                         for the device and the simulator
#   macos/Libraries/libfennec_ffi.dylib   arm64 and x86_64
#   android/src/main/jniLibs/<abi>/       libfennec_ffi.so for each ABI
#
# from what integrations/swift/build-xcframework.sh --dynamic made (the
# XCFramework and the dylib beside it, a release's
# FennecFFIDynamic.xcframework.zip) and integrations/kotlin/build-aar.sh.
# A platform asked for needs its artifacts: one missing is an error naming
# it, never a plugin without it. --platforms ios,macos,android (any subset,
# comma separated) prepares those alone and asks nothing of the others, as
# an Android-only job has no XCFramework; the default is every platform,
# what publishing needs, so a missing slice fails the publish.
#
# pub keeps no symbolic link -- it packs what one points at -- and every
# file here goes to pub.dev: a platform given another's slices, or a
# framework reached through links, is shipped again in full. CI packs the
# plugin as packages.yml does and holds the archive to that
# (tools/flutter_archive_check.py).
set -eu
PLATFORMS=ios,macos,android
case "${1:-}" in
  --platforms) [ $# -eq 2 ] || { echo "usage: prepare.sh [--platforms ios,macos,android]" >&2; exit 2; }
    PLATFORMS=$2 ;;
  "") ;;
  *) echo "usage: prepare.sh [--platforms ios,macos,android]" >&2; exit 2 ;;
esac
for p in $(echo "$PLATFORMS" | tr ',' ' '); do
  case "$p" in ios|macos|android) ;; *) echo "prepare.sh: unknown platform $p (ios, macos, android)" >&2; exit 2 ;; esac
done
want() { case ",$PLATFORMS," in *",$1,"*) return 0 ;; *) return 1 ;; esac; }
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
DYN="$ROOT/integrations/swift/build/dynamic"
JNI="$ROOT/integrations/kotlin/android/build/jniLibs"

if want ios; then
  [ -d "$DYN/FennecFFI.xcframework" ] || { echo "prepare.sh: $DYN/FennecFFI.xcframework is missing" >&2; exit 1; }
  src="$DYN/FennecFFI.xcframework"
  dst="$HERE/ios/Frameworks/FennecFFI.xcframework"
  rm -rf "$HERE/ios/Frameworks"
  mkdir -p "$dst"
  # The XCFramework's list of slices -- a <dict> each, as
  # build-xcframework.sh writes it -- without macOS's, then the slices it
  # still names. awk rather than a plist reader: this runs on Linux too.
  awk '
    /^\t\t<dict>$/ { entry = $0 "\n"; inside = 1; next }
    inside { entry = entry $0 "\n" }
    inside && /^\t\t<\/dict>$/ { inside = 0; if (entry !~ /<string>macos<\/string>/) printf "%s", entry; next }
    !inside { print }
  ' "$src/Info.plist" > "$dst/Info.plist"
  ids=$(sed -n '/<key>LibraryIdentifier<\/key>/{n;s/.*<string>\(.*\)<\/string>.*/\1/p;}' "$dst/Info.plist")
  for id in $ids; do cp -R "$src/$id" "$dst/"; done
  if [ -z "$ids" ]; then
    echo "prepare.sh: $src/Info.plist names no iOS slice" >&2
    exit 1
  fi
  for id in $ids; do
    [ -d "$dst/$id" ] || { echo "prepare.sh: slice $src/$id is missing" >&2; exit 1; }
  done
  echo "FennecFFI.xcframework -> ios/Frameworks: $(echo "$ids" | tr '\n' ' ')"
fi
if want macos; then
  [ -f "$DYN/libfennec_ffi.dylib" ] || { echo "prepare.sh: $DYN/libfennec_ffi.dylib is missing" >&2; exit 1; }
  rm -rf "$HERE/macos/Libraries"
  mkdir -p "$HERE/macos/Libraries"
  cp "$DYN/libfennec_ffi.dylib" "$HERE/macos/Libraries/"
  echo "libfennec_ffi.dylib -> macos/Libraries"
fi
if want android; then
  [ -d "$JNI" ] || { echo "prepare.sh: $JNI is missing" >&2; exit 1; }
  rm -rf "$HERE/android/src/main/jniLibs"
  cp -R "$JNI" "$HERE/android/src/main/jniLibs"
  echo "jniLibs -> android/src/main/jniLibs: $(ls "$HERE/android/src/main/jniLibs" | tr '\n' ' ')"
fi
