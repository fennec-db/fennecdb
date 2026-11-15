#!/bin/sh
# The C library for Linux, as release.yml makes it on a tag and CI's
# `linux (C library)` job makes it on every change before one:
#
#   tools/linux-ffi.sh VERSION TARGET DIST
#
# crates/fennec-ffi in the ffi profile -- panics unwind, and each call
# catches its own (docs/design/native-and-sync.md#the-native-library) -- for
# TARGET, x86_64-unknown-linux-gnu or aarch64-unknown-linux-gnu, on a runner
# of that architecture and against its glibc. Then
# DIST/fennec-ffi-vVERSION-TARGET.tar.gz, holding the folder of that name:
#
#   include/fennec.h                    crates/fennec-ffi/include's header
#   lib/libfennec_ffi.so                the shared library, its SONAME its
#                                       name, with the JNI functions the
#                                       Kotlin binding's JVM library calls
#   lib/libfennec_ffi.a                 the static library, its debug
#                                       sections stripped
#   lib/pkgconfig/fennec-ffi.pc         what links the shared library
#   lib/pkgconfig/fennec-ffi-static.pc  what links the static one
#   LICENSE, README.md
#
# The .pc files find the folder from where they are (${pcfiledir}), so it
# is used wherever it is unpacked. tools/linux-ffi-check.sh holds the
# tarball before it is kept. Run from anywhere in the repository.

set -eu

if [ $# -ne 3 ]; then
    echo "usage: tools/linux-ffi.sh VERSION TARGET DIST" >&2
    exit 2
fi
version=$1
target=$2
dist=$3
case $target in
    x86_64-unknown-linux-gnu | aarch64-unknown-linux-gnu) ;;
    *)
        echo "linux-ffi: $target is not a Linux target with glibc this script builds" >&2
        exit 2
        ;;
esac
root=$(cd "$(dirname "$0")/.." && pwd)
mkdir -p "$dist"
dist=$(cd "$dist" && pwd)
cd "$root"

# The library answers the crate's version (fennec_version) and the .pc
# files say the release's: the two are one, or the tarball is not built.
crate=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
if [ "$crate" != "$version" ]; then
    echo "linux-ffi: the workspace is version $crate, not $version" >&2
    exit 1
fi

stage=fennec-ffi-v$version-$target
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/$stage/include" "$work/$stage/lib/pkgconfig"

# Each library built alone, as `make ffi` builds one: beside the other
# crate types, link-time optimisation left the shared library 4% larger.
# The SONAME is the file's name, so a program records that name whether it
# was linked with -lfennec_ffi or by the file's path (CMake's way), and the
# loader looks for it on its own path.
cargo rustc -p fennec-ffi --lib --profile ffi --target "$target" --features jni \
    --crate-type cdylib -- -C link-arg=-Wl,-soname,libfennec_ffi.so
cp "target/$target/ffi/libfennec_ffi.so" "$work/$stage/lib/"
# The static library takes no JNI function: only the JVM loads one, and it
# loads the shared library.
cargo rustc -p fennec-ffi --lib --profile ffi --target "$target" --crate-type staticlib
cp "target/$target/ffi/libfennec_ffi.a" "$work/$stage/lib/"
# The profile strips what it links; an archive is not linked, and the
# debug information it carried was the standard library's.
strip --strip-debug "$work/$stage/lib/libfennec_ffi.a"

# What a program linking the static library links beside it. fennec-ffi
# takes no dependency (the zero-dependency rule), so it is the standard
# library's list, which rustc prints for any static library: an empty one
# here, since cargo prints nothing for a library it did not build again.
: >"$work/empty.rs"
libs=$(rustc --crate-type staticlib --crate-name empty --target "$target" \
    --print native-static-libs -o "$work/libempty.a" "$work/empty.rs" 2>&1 |
    sed -n 's/.*native-static-libs: //p')
if [ -z "$libs" ]; then
    echo "linux-ffi: rustc named no native library for $target" >&2
    exit 1
fi

cp crates/fennec-ffi/include/fennec.h "$work/$stage/include/"
cp LICENSE "$work/$stage/"

pc() {
    cat <<EOF
prefix=\${pcfiledir}/../..
includedir=\${prefix}/include
libdir=\${prefix}/lib

Name: $1
Description: fennecdb as a native library: the C ABI of crates/fennec-ffi$2
URL: https://fennecdb.com/docs/mobile#c-on-linux
Version: $version
Cflags: -I\${includedir}
Libs: $3
EOF
}
pc fennec-ffi "" "-L\${libdir} -lfennec_ffi" \
    >"$work/$stage/lib/pkgconfig/fennec-ffi.pc"
pc fennec-ffi-static ", linked in" "\${libdir}/libfennec_ffi.a $libs" \
    >"$work/$stage/lib/pkgconfig/fennec-ffi-static.pc"

cat >"$work/$stage/README.md" <<EOF
# fennecdb $version: the C library for $target

fennecdb's engine as a native library, its C ABI declared in
\`include/fennec.h\`: an app opens a file, runs FennecQL in it and keeps it
in step with a server, all through the calls declared there. It runs on
Linux with glibc 2.34 or later (Ubuntu 22.04, Debian 12, RHEL 9 and
their successors) and asks for nothing else but libgcc_s.

Point pkg-config at this folder, wherever it was unpacked:

    export PKG_CONFIG_PATH=\$PWD/$stage/lib/pkgconfig
    cc app.c \$(pkg-config --cflags --libs fennec-ffi)          # the shared library
    cc app.c \$(pkg-config --cflags --libs fennec-ffi-static)   # the static one, linked in

A program linked to the shared library finds \`libfennec_ffi.so\` where the
loader looks: copy it there, run the program with \`LD_LIBRARY_PATH\`
naming \`lib/\`, or link it with \`-Wl,-rpath,<folder>/lib\`. The library
carries the JNI functions too: Kotlin's \`com.fennecdb:fennecdb\` on the
JVM loads this file, named by \`-Dfennec.library\` or \`FENNEC_LIBRARY\`.

https://fennecdb.com/docs/mobile#c-on-linux has the rest.
EOF

rm -f "$dist/$stage.tar.gz"
tar --owner=0 --group=0 --numeric-owner -czf "$dist/$stage.tar.gz" -C "$work" "$stage"

echo "$stage.tar.gz  $(wc -c <"$dist/$stage.tar.gz" | tr -d ' ') bytes"
for f in libfennec_ffi.so libfennec_ffi.a; do
    echo "  lib/$f  $(wc -c <"$work/$stage/lib/$f" | tr -d ' ') bytes"
done
echo "  static: $libs"
