#!/bin/sh
# The Kotlin library's JVM tests (`make kotlin-test`): the native library
# built for Linux with its JNI functions, the fennec-server the sync tests
# start, kill and start again and the fennec-shard routers the read-routing
# tests start in front of it, then JUnit under Gradle -- the engine, the
# builder over every golden case, live queries as Flows, a replica against a
# real server, reads near a tenant's read replica.
#
# Where Gradle and Java are on the machine and it runs Linux (CI), both run
# here. Otherwise both run in containers -- the rust image of the release
# rust-toolchain.toml names, by digest, builds the library and the server
# for the container's Linux and gradle:8-jdk17 runs the tests -- with
# Cargo's and Gradle's caches in volumes of their own.
set -eu
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
GOLDEN=integrations/builder-golden.json

if [ "$(uname)" = Linux ] && command -v gradle >/dev/null 2>&1 && command -v java >/dev/null 2>&1; then
  CARGO=${CARGO:-$(test -x "$HOME/.cargo/bin/cargo" && echo "$HOME/.cargo/bin/cargo" || echo cargo)}
  (cd "$ROOT" && "$CARGO" rustc -q -p fennec-ffi --lib --features jni --profile ffi --crate-type cdylib)
  (cd "$ROOT" && "$CARGO" build -q -p fennec-server -p fennec-shard)
  cd "$ROOT/integrations/kotlin"
  FENNEC_LIBRARY="$ROOT/target/ffi/libfennec_ffi.so" FENNEC_GOLDEN="$ROOT/$GOLDEN" \
    FENNEC_SERVER="$ROOT/target/debug/fennec-server" FENNEC_SHARD="$ROOT/target/debug/fennec-shard" \
    exec gradle --no-daemon -q :fennecdb:test "$@"
fi

command -v docker >/dev/null 2>&1 || { echo "kotlin-test: needs Docker, or Linux with Gradle and Java" >&2; exit 1; }

# A target directory of its own: the host's target/ holds this machine's
# builds, and a Linux build in it would make Cargo build each again. The
# image is the release rust-toolchain.toml names (tools/toolchain_check.py
# holds the two alike), and its own toolchain is taken by name: under the
# file, rustup added the wasm32 target it asks for, at every run.
docker run --rm -v "$ROOT":/src -w /src \
  -v fennec-cargo-registry:/usr/local/cargo/registry \
  -e CARGO_TARGET_DIR=/src/target/linux-docker \
  rust:1.99.1-slim-bookworm@sha256:f2c004e3d2d8b547e8cf7ffbc29205385f4e71d23e53e4e383065db5e9f822cd \
  sh -c 'export RUSTUP_TOOLCHAIN=$(ls /usr/local/rustup/toolchains | head -1) &&
    cargo rustc -q -p fennec-ffi --lib --features jni --profile ffi --crate-type cdylib &&
    cargo build -q -p fennec-server -p fennec-shard'

docker run --rm -v "$ROOT":/src -w /src/integrations/kotlin \
  -v fennec-gradle:/home/gradle/.gradle \
  -e FENNEC_LIBRARY=/src/target/linux-docker/ffi/libfennec_ffi.so \
  -e FENNEC_GOLDEN=/src/$GOLDEN \
  -e FENNEC_SERVER=/src/target/linux-docker/debug/fennec-server \
  -e FENNEC_SHARD=/src/target/linux-docker/debug/fennec-shard \
  gradle:8-jdk17 \
  gradle --no-daemon -q :fennecdb:test "$@"
