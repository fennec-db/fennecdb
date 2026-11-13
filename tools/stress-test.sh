#!/bin/sh
# make stress-test: the Rust and JS suites ITER times inside
# `docker run --cpus=3` beside busy loops, which is GitHub's three-core
# runner and what docs/design/conventions.md#reruns says reproduces a flake.
#
#   tools/stress-test.sh                            # the workspace (but fennec-bench) and the JS tests, 20 times
#   ITER=2 CRATE=fennec-core tools/stress-test.sh   # one crate's tests, no JS
#
# Variables: ITER (20), CRATE (empty: --workspace --exclude fennec-bench and
# the JS tests), TIMEOUT (seconds for the whole run and for the build, 14400),
# BUSY (busy loops, 3), CPUS (3), IMAGE (the rust image of rust-toolchain.toml's
# release, by digest, as tools/toolchain_check.py holds every image),
# LOGDIR (a new directory under /tmp).
#
# Why everything runs in the container: the load is busy loops and many
# loopback clients, and on the host they once exhausted its ports and cut
# every agent off the API. The container's loopback is its own. The busy
# loops are started here and killed by PID on every way out (exit, Ctrl-C,
# a failed test); the container is --init and removed with --rm, and the
# host side kills it by name if interrupted.
#
# Node: the JS tests run on the node 22 CI runs, not Debian's 18: the
# official tarball, checked against its published SHASUMS256 (pinned below).
#
# Cache: a Docker volume, named for the architecture and the image's digest
# (a toolchain's target directory is no other's) and locked while a run uses
# it, holds the cargo home, the target directory and a
# copy of the sources. The copy is made by content (rsync -c), as
# tools/image-cargo.sh does: cargo takes an output as fresh when its mtime
# is later than its sources', so a bind mount of a worktree whose files are
# older than the last build would compile nothing.
#
# It stops after the first iteration that fails (every failing test binary of
# it is run, --no-fail-fast), keeps that iteration's log and prints the
# iteration and the failing tests. Exit: 0 all passed, 1 a failure, 2 the
# timeout or a setup error.
set -u

ITER="${ITER:-20}"
CRATE="${CRATE:-}"
TIMEOUT="${TIMEOUT:-14400}"
BUSY="${BUSY:-3}"
CPUS="${CPUS:-3}"
# rust:1.99.1-bookworm's multi-arch index digest; a bump goes with
# rust-toolchain.toml (docker buildx imagetools inspect rust:X.Y.Z-bookworm).
IMAGE="${IMAGE:-rust:1.99.1-bookworm@sha256:e6f2cff35d0f309acf8433b4f05878ed77092602c5065cc81801177da0bb3a9c}"
# CI's node major, from nodejs.org/dist with the sums of its SHASUMS256.txt.
NODE_VERSION=22.23.5
NODE_SHA_X64=cdeb8670a0c81f51b4dc4ac61919f9b1ad917b304bf8221eb939570663d240f8
NODE_SHA_ARM64=2babb9eb5264860d540849f4b064a6fbdebcdc323657d9d382ce23d313fd5e09

if [ "${STRESS_INNER:-}" != 1 ]; then
  root=$(cd "$(dirname "$0")/.." && pwd)
  logdir="${LOGDIR:-$(mktemp -d /tmp/fennec-stress.XXXXXX)}"
  mkdir -p "$logdir"
  name="fennec-stress-$$"
  command -v docker >/dev/null 2>&1 || { echo "stress-test needs docker" >&2; exit 2; }
  # Only this run's container is killed, by the name it was given. The
  # container runs in the background and is waited for, because a shell
  # runs a trap only once its foreground command is over: a TERM would
  # otherwise leave the container running.
  trap 'docker kill "$name" >/dev/null 2>&1; exit 130' INT TERM
  case "$IMAGE" in
    *@sha256:*) short=$(printf '%s' "${IMAGE##*@sha256:}" | cut -c1-12) ;;
    *) short=$(printf '%s' "$IMAGE" | tr -c 'A-Za-z0-9\n' '-') ;;
  esac
  volume="fennec-stress-$(uname -m)-$short"
  echo "stress-test: ITER=$ITER CRATE=${CRATE:-workspace} cpus=$CPUS busy=$BUSY cache=$volume logs=$logdir"
  docker run --rm --init --name "$name" --cpus="$CPUS" \
    -v "$root":/src:ro \
    -v "$volume":/cache \
    -v "$logdir":/logs \
    -e STRESS_INNER=1 -e ITER="$ITER" -e CRATE="$CRATE" -e TIMEOUT="$TIMEOUT" \
    -e BUSY="$BUSY" -e NODE_VERSION="$NODE_VERSION" \
    -e NODE_SHA_X64="$NODE_SHA_X64" -e NODE_SHA_ARM64="$NODE_SHA_ARM64" \
    "$IMAGE" sh /src/tools/stress-test.sh &
  wait $!
  rc=$?
  [ "$rc" = 0 ] || echo "stress-test: failed (exit $rc); logs in $logdir"
  exit "$rc"
fi

# Inside the container.
busy_pids=""
cleanup() {
  for p in $busy_pids; do kill "$p" 2>/dev/null; done
  for p in $busy_pids; do wait "$p" 2>/dev/null; done
  busy_pids=""
}
trap 'cleanup' EXIT
trap 'cleanup; exit 130' INT TERM

export CARGO_HOME=/cache/cargo CARGO_TARGET_DIR=/cache/target CARGO_TERM_COLOR=never
mkdir -p /cache/src

# Two runs on one volume would race on /cache/src and the target directory;
# the lock goes with the process, so a killed run leaves none behind.
if command -v flock >/dev/null 2>&1; then
  exec 9>/cache/lock
  flock -n 9 || { echo "stress-test: another run holds this cache volume" >&2; exit 2; }
fi

need=""
command -v rsync >/dev/null 2>&1 || need="$need rsync"
command -v make >/dev/null 2>&1 || need="$need make"
command -v xz >/dev/null 2>&1 || need="$need xz-utils"
if [ -n "$need" ]; then
  apt-get update -qq >/dev/null && apt-get install -y -qq --no-install-recommends $need >/dev/null \
    || { echo "stress-test: could not install$need" >&2; exit 2; }
fi
if [ -z "$CRATE" ]; then
  case "$(uname -m)" in
    x86_64) node_arch=x64 node_sha=$NODE_SHA_X64 ;;
    aarch64) node_arch=arm64 node_sha=$NODE_SHA_ARM64 ;;
    *) echo "stress-test: no node for $(uname -m)" >&2; exit 2 ;;
  esac
  node_dir=/cache/node-v$NODE_VERSION-linux-$node_arch
  if [ ! -x "$node_dir/bin/node" ]; then
    tgz=/cache/node-$NODE_VERSION.tar.xz
    curl -fsSL "https://nodejs.org/dist/v$NODE_VERSION/node-v$NODE_VERSION-linux-$node_arch.tar.xz" -o "$tgz" \
      && [ "$(sha256sum "$tgz" | cut -d' ' -f1)" = "$node_sha" ] \
      && tar -xJf "$tgz" -C /cache \
      || { rm -f "$tgz"; echo "stress-test: could not fetch node $NODE_VERSION" >&2; exit 2; }
    rm -f "$tgz"
  fi
  export PATH="$node_dir/bin:$PATH"
fi
rsync -rc --delete --exclude /target --exclude .git --exclude node_modules /src/ /cache/src/ \
  || { echo "stress-test: could not copy the sources" >&2; exit 2; }
cd /cache/src || exit 2
ln -sfn /cache/target target
# Not silenced: it installs rust-toolchain.toml's release and target, and a
# failure here is the run's, not a later cargo's puzzle.
rustup show >/logs/versions.log 2>&1 || { cat /logs/versions.log >&2; echo "stress-test: rustup show failed" >&2; exit 2; }
{ rustc -V; [ -z "$CRATE" ] && node --version; } | tee -a /logs/versions.log

if [ -n "$CRATE" ]; then
  rust_cmd="cargo test --no-fail-fast -p $CRATE"
else
  rust_cmd="cargo test --no-fail-fast --workspace --exclude fennec-bench"
  # The JS tests start fennec-server and use the module; build both once.
  timeout "$TIMEOUT" cargo build -p fennec-server || exit 2
  timeout "$TIMEOUT" make wasm >/dev/null || echo "stress-test: no web/fennec.wasm; the JS tests skip what needs it" >&2
fi

# Build before the load starts, so the iterations measure the tests.
timeout "$TIMEOUT" $rust_cmd --no-run >/logs/build.log 2>&1 || { tail -30 /logs/build.log; echo "stress-test: build failed" >&2; exit 2; }

i=0
while [ "$i" -lt "$BUSY" ]; do
  (while :; do :; done) &
  busy_pids="$busy_pids $!"
  i=$((i + 1))
done

start=$(date +%s)
n=1
while [ "$n" -le "$ITER" ]; do
  left=$((TIMEOUT - ($(date +%s) - start)))
  if [ "$left" -le 0 ]; then echo "stress-test: timeout after $((n - 1)) iterations" >&2; exit 2; fi
  log=/logs/iter-$n-rust.log
  timeout "$left" $rust_cmd >"$log" 2>&1
  rc=$?
  if [ "$rc" != 0 ]; then
    [ "$rc" = 124 ] && { echo "stress-test: timeout in iteration $n (Rust)" >&2; exit 2; }
    echo "stress-test: FAILED iteration $n/$ITER (Rust), log $log"
    grep -E '^test .* FAILED|^failures:|panicked at|error: test failed' "$log" | head -20
    exit 1
  fi
  if [ -z "$CRATE" ]; then
    log=/logs/iter-$n-js.log
    left=$((TIMEOUT - ($(date +%s) - start)))
    [ "$left" -le 0 ] && { echo "stress-test: timeout in iteration $n" >&2; exit 2; }
    timeout "$left" node --test --test-reporter=spec web/*.test.js >"$log" 2>&1
    rc=$?
    if [ "$rc" != 0 ]; then
      [ "$rc" = 124 ] && { echo "stress-test: timeout in iteration $n (JS)" >&2; exit 2; }
      echo "stress-test: FAILED iteration $n/$ITER (JS), log $log"
      grep -E '^\s*(✖|not ok)' "$log" | head -20
      exit 1
    fi
  fi
  echo "stress-test: iteration $n/$ITER passed ($(( $(date +%s) - start )) s)"
  # Passing logs are not kept: 20 iterations write hundreds of megabytes.
  rm -f "/logs/iter-$n-rust.log" "/logs/iter-$n-js.log"
  n=$((n + 1))
done
echo "stress-test: $ITER/$ITER passed"
