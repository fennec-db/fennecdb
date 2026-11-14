#!/bin/sh
# crates/fennec-ffi as an XCFramework for each Apple platform, in
# integrations/swift/build/:
#
#   FennecFFI.xcframework        a static library a slice, with the header and
#                                a module map: what Package.swift takes in
#                                place of the release's download
#   dynamic/FennecFFI.xcframework  (--dynamic) FennecFFI.framework a slice,
#                                the shared library: what an app that
#                                embeds the framework itself takes, and
#                                the Flutter plugin's iOS pod vendors
#   dynamic/libfennec_ffi.dylib  macOS's framework binary as a plain dylib:
#                                what the plugin's macOS pod vendors --
#                                zipped with the XCFramework as
#                                FennecFFIDynamic
#
#   integrations/swift/build-xcframework.sh            macOS, iOS, the simulator
#   integrations/swift/build-xcframework.sh --macos    macOS alone, this machine's arch
#   integrations/swift/build-xcframework.sh --dynamic  the frameworks and the dylib instead
#   integrations/swift/build-xcframework.sh --zip      and the zip a release
#                                                      attaches (the static one
#                                                      with its checksum)
#
# Assembled by hand -- the Info.plist is a list of the slices -- rather
# than by `xcodebuild -create-xcframework`, which needs Xcode: the
# Command Line Tools build and test the macOS slices with this alone. Rust
# builds a static library without linking it, so the static iOS slices
# build without the iOS SDK too; the dynamic ones are linked, and need it.
#
# Why two. Swift links the static library into the app, which is what an
# app ships. The Flutter plugin's Dart side finds the functions by name at
# run time, where nothing in the app calls them, so a static library had to
# be kept whole with -force_load at the app's link -- and that pointed the
# Runner at a file CocoaPods' "Copy XCFrameworks" phase makes with no order
# declared against it: `flutter build ios` stopped at "Build input file
# cannot be found". A dynamic framework is what Flutter's own docs give an
# FFI plugin with a prebuilt binary: CocoaPods embeds it, nothing is
# force-loaded, and Dart opens `FennecFFI.framework/FennecFFI`.
#
# Why the dylib. pub keeps no symbolic link: it packs what one points at. A
# macOS framework is versioned -- its binary in Versions/A, reached through
# Versions/Current and the top level -- so the plugin went out with that
# binary three times in each copy of the XCFramework (0.2.0: 34.5 MB
# packed, 65.7 MB unpacked). On macOS the plugin vendors one file instead,
# which CocoaPods embeds and signs as it does a framework and Dart opens by
# its name; the XCFramework keeps its macOS framework for the apps that
# embed it themselves. No XCFramework can hold the dylib beside the
# frameworks: `xcodebuild -create-xcframework` refuses frameworks and
# libraries in one.
set -eu

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
OUT="$ROOT/integrations/swift/build"
CARGO=${CARGO:-$(test -x "$HOME/.cargo/bin/cargo" && echo "$HOME/.cargo/bin/cargo" || echo cargo)}
RUSTUP=${RUSTUP:-$(test -x "$HOME/.cargo/bin/rustup" && echo "$HOME/.cargo/bin/rustup" || echo rustup)}
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)
# The platforms Package.swift and the pods declare: the objects say so too,
# rather than Rust's own defaults. macOS 11 is the lowest
# aarch64-apple-darwin builds for; an app built for it asked for 11, where
# the objects said 12.
export IPHONEOS_DEPLOYMENT_TARGET=15.0 MACOSX_DEPLOYMENT_TARGET=11.0
# A stale target/ keeps objects built for 12.0, which cargo does not rebuild
# when only this variable changes: the library's own objects go first.
"$CARGO" clean -p fennec-ffi

macos_only=false
zip=false
dynamic=false
for a in "$@"; do
  case "$a" in
    --macos) macos_only=true ;;
    --zip) zip=true ;;
    --dynamic) dynamic=true ;;
    *) echo "usage: $0 [--macos] [--dynamic] [--zip]" >&2; exit 2 ;;
  esac
done
# CocoaPods links a vendored XCFramework by its own name, so the dynamic one
# is called FennecFFI.xcframework too, as the frameworks inside it are --
# named FennecFFIDynamic, the app's link asked for a framework of that name.
# It is built in a directory of its own beside the static one, and only its
# zip carries the other name.
NAME=FennecFFI
if $dynamic; then XDIR="$OUT/dynamic"; ZIPNAME=FennecFFIDynamic; else XDIR="$OUT"; ZIPNAME=FennecFFI; fi
mkdir -p "$XDIR"
XC="$XDIR/$NAME.xcframework"
DYLIB="$XDIR/libfennec_ffi.dylib"

# One static library a target, the lib alone: `cargo build` would link the
# cdylib too, which for iOS needs its SDK.
static_lib() {
  "$RUSTUP" target add "$1" >/dev/null 2>&1 || true
  (cd "$ROOT" && "$CARGO" rustc -q -p fennec-ffi --lib --profile ffi --target "$1" --crate-type staticlib)
  echo "$ROOT/target/$1/ffi/libfennec_ffi.a"
}

# One shared library a target, its install name the framework's binary, so
# the app's loader finds it through @rpath in its Frameworks directory.
shared_lib() {
  "$RUSTUP" target add "$1" >/dev/null 2>&1 || true
  (cd "$ROOT" && "$CARGO" rustc -q -p fennec-ffi --lib --profile ffi --target "$1" --crate-type cdylib \
    -- -C "link-arg=-Wl,-install_name,$2")
  echo "$ROOT/target/$1/ffi/libfennec_ffi.dylib"
}

# A static slice: the library (several architectures made one with lipo),
# the header and the module map Swift imports it by.
static_slice() {
  id=$1
  shift
  mkdir -p "$XC/$id/Headers"
  libs=""
  for t in "$@"; do libs="$libs $(static_lib "$t")"; done
  # shellcheck disable=SC2086
  lipo -create $libs -output "$XC/$id/libfennec_ffi.a"
  cp "$ROOT/crates/fennec-ffi/include/fennec.h" "$XC/$id/Headers/"
  cat > "$XC/$id/Headers/module.modulemap" <<'EOF'
module FennecFFI {
    header "fennec.h"
    export *
}
EOF
  echo "$id  $(wc -c < "$XC/$id/libfennec_ffi.a") bytes (static, unlinked)" >&2
}

# A framework's Info.plist: the bundle's name, its binary and the lowest
# system it runs on, which App Store validation reads.
framework_plist() {
  platform=$1 minimum=$2
  cat <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key>
	<string>en</string>
	<key>CFBundleExecutable</key>
	<string>FennecFFI</string>
	<key>CFBundleIdentifier</key>
	<string>com.fennecdb.FennecFFI</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleName</key>
	<string>FennecFFI</string>
	<key>CFBundlePackageType</key>
	<string>FMWK</string>
	<key>CFBundleShortVersionString</key>
	<string>$VERSION</string>
	<key>CFBundleVersion</key>
	<string>$VERSION</string>
	<key>CFBundleSupportedPlatforms</key>
	<array>
		<string>$platform</string>
	</array>
	$minimum
</dict>
</plist>
EOF
}

# A dynamic slice: FennecFFI.framework. iOS's is flat, the binary and the
# Info.plist side by side; macOS's is versioned (Versions/A, Current and
# the links to it), which code signing there requires.
dynamic_slice() {
  id=$1 platform=$2
  shift 2
  fw="$XC/$id/FennecFFI.framework"
  case "$platform" in
    MacOSX)
      mkdir -p "$fw/Versions/A/Resources"
      install="@rpath/FennecFFI.framework/Versions/A/FennecFFI"
      bin="$fw/Versions/A/FennecFFI"
      plist="$fw/Versions/A/Resources/Info.plist"
      minimum="<key>LSMinimumSystemVersion</key>
	<string>$MACOSX_DEPLOYMENT_TARGET</string>"
      ;;
    *)
      mkdir -p "$fw"
      install="@rpath/FennecFFI.framework/FennecFFI"
      bin="$fw/FennecFFI"
      plist="$fw/Info.plist"
      minimum="<key>MinimumOSVersion</key>
	<string>$IPHONEOS_DEPLOYMENT_TARGET</string>"
      ;;
  esac
  libs=""
  for t in "$@"; do libs="$libs $(shared_lib "$t" "$install")"; done
  # shellcheck disable=SC2086
  lipo -create $libs -output "$bin"
  framework_plist "$platform" "$minimum" > "$plist"
  if [ "$platform" = MacOSX ]; then
    ln -s A "$fw/Versions/Current"
    ln -s Versions/Current/FennecFFI "$fw/FennecFFI"
    ln -s Versions/Current/Resources "$fw/Resources"
  fi
  echo "$id  $(wc -c < "$bin") bytes (FennecFFI.framework)" >&2
}

# The macOS slice's framework binary as a plain dylib, its install name its
# own, which the app finds through @rpath in its Frameworks directory. Named
# anew rather than linked twice; a new name voids the linker's ad-hoc
# signature, so it is taken off first and the dylib signed again.
macos_dylib() {
  cp "$XC/$1/FennecFFI.framework/Versions/A/FennecFFI" "$DYLIB"
  codesign --remove-signature "$DYLIB"
  install_name_tool -id @rpath/libfennec_ffi.dylib "$DYLIB"
  codesign --sign - "$DYLIB"
  echo "macos  $(wc -c < "$DYLIB") bytes (libfennec_ffi.dylib)" >&2
}

slice() {
  if $dynamic; then dynamic_slice "$@"; else
    id=$1
    shift 2
    static_slice "$id" "$@"
  fi
}

# One entry of Info.plist's AvailableLibraries.
entry() {
  id=$1 platform=$2 variant=$3
  shift 3
  if $dynamic; then
    lib=FennecFFI.framework
    case "$platform" in
      macos) binary=FennecFFI.framework/Versions/A/FennecFFI ;;
      *) binary=FennecFFI.framework/FennecFFI ;;
    esac
  else
    lib=libfennec_ffi.a binary=libfennec_ffi.a
  fi
  printf '\t\t<dict>\n'
  printf '\t\t\t<key>BinaryPath</key>\n\t\t\t<string>%s</string>\n' "$binary"
  if ! $dynamic; then
    printf '\t\t\t<key>HeadersPath</key>\n\t\t\t<string>Headers</string>\n'
  fi
  printf '\t\t\t<key>LibraryIdentifier</key>\n\t\t\t<string>%s</string>\n' "$id"
  printf '\t\t\t<key>LibraryPath</key>\n\t\t\t<string>%s</string>\n' "$lib"
  printf '\t\t\t<key>SupportedArchitectures</key>\n\t\t\t<array>\n'
  for arch in "$@"; do printf '\t\t\t\t<string>%s</string>\n' "$arch"; done
  printf '\t\t\t</array>\n'
  printf '\t\t\t<key>SupportedPlatform</key>\n\t\t\t<string>%s</string>\n' "$platform"
  if [ -n "$variant" ]; then
    printf '\t\t\t<key>SupportedPlatformVariant</key>\n\t\t\t<string>%s</string>\n' "$variant"
  fi
  printf '\t\t</dict>\n'
}

rm -rf "$XC"
if $dynamic; then rm -f "$DYLIB"; fi
mkdir -p "$XC"
if $macos_only; then
  # This machine's architecture alone: what `swift test` links.
  case "$(uname -m)" in
    arm64) host=aarch64-apple-darwin arch=arm64 ;;
    *) host=x86_64-apple-darwin arch=x86_64 ;;
  esac
  slice "macos-$arch" MacOSX "$host"
  entries=$(entry "macos-$arch" macos "" "$arch")
  macos="macos-$arch"
else
  slice macos-arm64_x86_64 MacOSX aarch64-apple-darwin x86_64-apple-darwin
  slice ios-arm64 iPhoneOS aarch64-apple-ios
  slice ios-arm64_x86_64-simulator iPhoneSimulator aarch64-apple-ios-sim x86_64-apple-ios
  entries=$(
    entry macos-arm64_x86_64 macos "" arm64 x86_64
    entry ios-arm64 ios "" arm64
    entry ios-arm64_x86_64-simulator ios simulator arm64 x86_64
  )
  macos="macos-arm64_x86_64"
fi
if $dynamic; then macos_dylib "$macos"; fi

cat > "$XC/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>AvailableLibraries</key>
	<array>
$entries
	</array>
	<key>CFBundlePackageType</key>
	<string>XFWK</string>
	<key>XCFrameworkFormatVersion</key>
	<string>1.0</string>
</dict>
</plist>
EOF
echo "$XC"
if $dynamic; then echo "$DYLIB"; fi

if $zip; then
  # The zip a release attaches. -X leaves out the extra attributes, and -y
  # keeps a macOS framework's links as links. The static one's checksum is
  # what Package.swift names it by; the dynamic one, the XCFramework and
  # the dylib, is named by nothing.
  rm -f "$OUT/$ZIPNAME.xcframework.zip"
  if $dynamic; then members="$NAME.xcframework libfennec_ffi.dylib"; else members="$NAME.xcframework"; fi
  # shellcheck disable=SC2086
  (cd "$XDIR" && zip -qrXy "$OUT/$ZIPNAME.xcframework.zip" $members)
  echo "$OUT/$ZIPNAME.xcframework.zip  $(wc -c < "$OUT/$ZIPNAME.xcframework.zip") bytes"
  if ! $dynamic; then
    sum=$(swift package compute-checksum "$OUT/$ZIPNAME.xcframework.zip")
    echo "$sum" > "$OUT/$ZIPNAME.xcframework.zip.checksum"
    echo "checksum $sum"
  fi
fi
