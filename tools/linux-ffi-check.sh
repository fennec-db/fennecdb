#!/bin/sh
# The C library's tarball for Linux, held before it is kept, as
# tools/linux-packages-check.sh holds a package:
#
#   tools/linux-ffi-check.sh VERSION TARGET DIST
#
# DIST holds what tools/linux-ffi.sh wrote. The tarball holds its files and
# nothing else, its header the repository's. The shared library is an ELF of
# TARGET's machine whose SONAME is its name, asking for nothing but glibc
# and libgcc_s and for no glibc symbol newer than GLIBC_MAX -- the oldest
# glibc the docs say it runs on -- and exporting every function the header
# declares, the JNI functions the Kotlin binding declares, and nothing else;
# the static one defines every function the header declares; each .pc file
# says VERSION. Then the header is compiled as C++, and
# tools/linux-ffi-check.c as C99 with every warning an error, linked to the
# shared library through `pkg-config --cflags --libs fennec-ffi` and to the
# static one through `fennec-ffi-static` -- the lines the docs give, run as
# given -- and both programs are run here and in AlmaLinux 9, whose glibc is
# GLIBC_MAX. Every check that fails says what it found and what it wanted.
# Run on a runner of TARGET's architecture, with Docker, from anywhere in
# the repository.

set -eu

GLIBC_MAX=2.34
CONTAINER=almalinux:9

if [ $# -ne 3 ]; then
    echo "usage: tools/linux-ffi-check.sh VERSION TARGET DIST" >&2
    exit 2
fi
version=$1
target=$2
case $target in
    x86_64-unknown-linux-gnu) machine="Advanced Micro Devices X86-64" ;;
    aarch64-unknown-linux-gnu) machine="AArch64" ;;
    *)
        echo "linux-ffi-check: $target is not a target tools/linux-ffi.sh builds" >&2
        exit 2
        ;;
esac
root=$(cd "$(dirname "$0")/.." && pwd)
dist=$(cd "$3" && pwd)
stage=fennec-ffi-v$version-$target
tarball=$dist/$stage.tar.gz

fail() {
    echo "linux-ffi-check: $*" >&2
    exit 1
}
ok() { echo "ok: $*"; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
[ -f "$tarball" ] || fail "no $tarball"

# Its files, and nothing else.
tar -tzf "$tarball" | grep -v '/$' | sort >"$work/have"
for f in LICENSE README.md include/fennec.h lib/libfennec_ffi.a lib/libfennec_ffi.so \
    lib/pkgconfig/fennec-ffi-static.pc lib/pkgconfig/fennec-ffi.pc; do
    echo "$stage/$f"
done | sort >"$work/want"
diff -u "$work/want" "$work/have" >&2 || fail "$stage.tar.gz holds other files than these (- wanted, + found)"
tar -xzf "$tarball" -C "$work"
dir=$work/$stage
so=$dir/lib/libfennec_ffi.so
a=$dir/lib/libfennec_ffi.a
cmp "$dir/include/fennec.h" "$root/crates/fennec-ffi/include/fennec.h" >/dev/null ||
    fail "include/fennec.h is not crates/fennec-ffi/include/fennec.h"
ok "the tarball's files, its header the repository's"

# The shared library: its machine, its name, what it asks the system for.
got=$(readelf -h "$so" | sed -n 's/^ *Machine: *//p')
[ "$got" = "$machine" ] || fail "libfennec_ffi.so is for $got, not $machine"
got=$(readelf -d "$so" | sed -n 's/.*(SONAME).*\[\(.*\)\]/\1/p')
[ "$got" = libfennec_ffi.so ] || fail "libfennec_ffi.so's SONAME is '$got', not libfennec_ffi.so"
readelf -d "$so" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p' >"$work/needed"
while read -r lib; do
    case $lib in
        libc.so.6 | libm.so.6 | libgcc_s.so.1 | ld-linux-x86-64.so.2 | ld-linux-aarch64.so.1 | \
            libpthread.so.0 | libdl.so.2 | librt.so.1 | libutil.so.1) ;;
        *) fail "libfennec_ffi.so asks for $lib; it may ask for glibc's and libgcc_s alone: $(tr '\n' ' ' <"$work/needed")" ;;
    esac
done <"$work/needed"
newest=$(objdump -T "$so" | grep -o 'GLIBC_[0-9][0-9.]*' | sed 's/^GLIBC_//' | sort -u -V | tail -n 1)
[ -n "$newest" ] || fail "libfennec_ffi.so names no glibc symbol version"
[ "$(printf '%s\n%s\n' "$newest" "$GLIBC_MAX" | sort -V | tail -n 1)" = "$GLIBC_MAX" ] ||
    fail "libfennec_ffi.so asks for GLIBC_$newest, newer than the $GLIBC_MAX the docs say it runs on: $(objdump -T "$so" | grep "GLIBC_$newest" | awk '{ print $NF }' | tr '\n' ' ')"
ok "libfennec_ffi.so: $got for $machine, asking for $(tr '\n' ' ' <"$work/needed")and glibc $newest at most"

# What it exports: the header's functions, the Kotlin binding's JNI
# functions, and nothing else.
grep -o 'fennec_[a-z0-9_]*(' "$dir/include/fennec.h" | tr -d '(' | sort -u >"$work/declared"
[ -s "$work/declared" ] || fail "fennec.h declares no function"
sed -n 's/.*external fun \([A-Za-z0-9]*\).*/Java_com_fennecdb_FennecNative_\1/p' \
    "$root/integrations/kotlin/fennecdb/src/main/kotlin/com/fennecdb/FennecNative.kt" | sort -u >"$work/jni"
[ -s "$work/jni" ] || fail "FennecNative.kt declares no external function"
nm -D --defined-only "$so" | awk '$2 == "T" { sub(/@.*/, "", $3); print $3 }' | sort -u >"$work/exported"
missing=$(sort -u "$work/declared" "$work/jni" | comm -23 - "$work/exported")
[ -z "$missing" ] || fail "libfennec_ffi.so does not export $(echo "$missing" | tr '\n' ' ')"
other=$(grep -v -e '^fennec_' -e '^Java_com_fennecdb_FennecNative_' "$work/exported" || true)
[ -z "$other" ] || fail "libfennec_ffi.so exports $(echo "$other" | tr '\n' ' ')beside the header's and the JNI functions"
ok "libfennec_ffi.so exports the header's $(wc -l <"$work/declared" | tr -d ' ') functions and Kotlin's $(wc -l <"$work/jni" | tr -d ' ') JNI functions alone"

nm --defined-only "$a" 2>/dev/null | awk '$2 == "T" { print $3 }' | sort -u >"$work/static"
missing=$(comm -23 "$work/declared" "$work/static")
[ -z "$missing" ] || fail "libfennec_ffi.a does not define $(echo "$missing" | tr '\n' ' ')"
ok "libfennec_ffi.a defines the header's functions"

# The pkg-config files, then the lines the docs give.
PKG_CONFIG_PATH=$dir/lib/pkgconfig
export PKG_CONFIG_PATH
for m in fennec-ffi fennec-ffi-static; do
    got=$(pkg-config --modversion "$m") || fail "pkg-config does not read $m.pc"
    [ "$got" = "$version" ] || fail "$m.pc says version $got, not $version"
done
ok "fennec-ffi.pc and fennec-ffi-static.pc: version $version"

printf '#include <fennec.h>\nint main() { return fennec_version() == nullptr; }\n' >"$work/header.cc"
# shellcheck disable=SC2046 # pkg-config's words are flags, one each
c++ -std=c++11 -Wall -Wextra -Werror -pedantic -fsyntax-only $(pkg-config --cflags fennec-ffi) "$work/header.cc" ||
    fail "fennec.h does not compile as C++"
ok "fennec.h compiles as C++"

run=$work/run
mkdir -p "$run/lib"
cp "$so" "$run/lib/"
flags="-std=c99 -Wall -Wextra -Werror -pedantic -O2"
# shellcheck disable=SC2046,SC2086 # flags, a word each
cc $flags -o "$run/check-shared" "$root/tools/linux-ffi-check.c" $(pkg-config --cflags --libs fennec-ffi) ||
    fail "tools/linux-ffi-check.c does not build against the shared library"
# shellcheck disable=SC2046,SC2086
cc $flags -o "$run/check-static" "$root/tools/linux-ffi-check.c" $(pkg-config --cflags --libs fennec-ffi-static) ||
    fail "tools/linux-ffi-check.c does not build against the static library"
readelf -d "$run/check-shared" | grep -q 'NEEDED.*\[libfennec_ffi\.so\]' ||
    fail "check-shared does not ask for libfennec_ffi.so: $(readelf -d "$run/check-shared" | grep NEEDED)"
if readelf -d "$run/check-static" | grep -q 'NEEDED.*fennec'; then
    fail "check-static asks for the shared library: $(readelf -d "$run/check-static" | grep NEEDED)"
fi
ok "a C program linked to each: $(wc -c <"$run/check-shared" | tr -d ' ') bytes with the shared library, $(wc -c <"$run/check-static" | tr -d ' ') with the static one"

for p in check-shared check-static; do
    mkdir "$work/$p.data"
    echo "-- $p, glibc $(getconf GNU_LIBC_VERSION | sed 's/^glibc //')"
    LD_LIBRARY_PATH=$run/lib "$run/$p" "$version" "$work/$p.data" || fail "$p failed here"
done

# Where the glibc is the oldest the docs name: the programs built here, the
# library beside them, nothing installed in the container.
echo "-- in $CONTAINER"
docker run --rm -v "$run:/w:ro" -e LD_LIBRARY_PATH=/w/lib "$CONTAINER" sh -c '
    set -eu
    have=$(getconf GNU_LIBC_VERSION)
    [ "$have" = "glibc $2" ] || { echo "the container has $have, not glibc $2" >&2; exit 1; }
    for p in check-shared check-static; do
        echo "-- $p, $have"
        mkdir "/tmp/$p"
        "/w/$p" "$1" "/tmp/$p"
    done' sh "$version" "$GLIBC_MAX" || fail "the programs failed in $CONTAINER"
ok "$stage.tar.gz: linked, run here and on glibc $GLIBC_MAX"
