#!/bin/sh
# The Linux binaries the runs under a memory limit use inside containers,
# built into the Docker volume <prefix>-tools, which holds them at its root:
#
#   ycsb     fennecdb in process under a container's --memory
#            (run-larger-than-memory.sh), built without the rivals'
#            crates (`--no-default-features`)
#   fennec   `fennec verify`, `backup` and `restore` against a server's
#            volume (run-soak.sh), `backup` alone of its features
#
#   crates/fennec-bench/ycsb/linux-tools.sh p6
#
# Built in the rust image the server's own Dockerfile builds in, by the
# digest its FROM names -- so static musl binaries for the VM's
# architecture, and the image the pre-flight found (ycsb/env-check.sh) --
# from this checkout mounted read-only, with no network: neither binary
# takes a crate from outside the workspace, and cargo resolves the lock
# file from the host's registry index (~/.cargo/registry, mounted
# read-only), so nothing is fetched and nothing is pulled. The target
# directory stays in the volume, a build after the first compiling what
# changed; the scripts that call this remove the volume when they end.
set -eu
prefix=${1:?usage: linux-tools.sh <prefix>}
cd "$(dirname "$0")/../../.."
root=$(pwd)
registry=${CARGO_HOME:-$HOME/.cargo}/registry
[ -d "$registry/index" ] || {
    echo "no cargo registry index at $registry: build the workspace once on this machine first" >&2
    exit 1
}
image=$(awk '/^FROM rust:[^ ]*@sha256:/ {print $2; exit}' Dockerfile)
docker image inspect "$image" >/dev/null 2>&1 || {
    echo "the image $image is not on this machine (by its digest): docker pull $image" >&2
    exit 1
}
docker volume create "$prefix-tools" >/dev/null
# rust-toolchain.toml names the release this image is and a wasm32 target
# no binary here needs, which rustup would fetch: RUSTUP_TOOLCHAIN takes
# the image's own toolchain instead.
docker run --rm --pull never --network none --name "$prefix-build" \
    -e RUSTUP_TOOLCHAIN="$(sed -n 's/^channel *= *"\([^"]*\)".*/\1/p' rust-toolchain.toml)" \
    -e CARGO_NET_OFFLINE=true \
    -e CARGO_TARGET_DIR=/tools/target \
    -v "$root":/src:ro \
    -v "$registry":/usr/local/cargo/registry:ro \
    -v "$prefix-tools":/tools \
    -w /src "$image" sh -c '
        cargo build --release --locked --offline -q \
            -p fennec-bench --bin ycsb --no-default-features &&
        cargo build --release --locked --offline -q \
            -p fennec-cli --bin fennec --no-default-features --features backup &&
        cp /tools/target/release/ycsb /tools/target/release/fennec /tools/ &&
        /tools/fennec --version'
