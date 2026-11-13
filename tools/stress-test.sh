#!/bin/sh
# make stress-test: the Rust and JS suites ITER times inside
# `docker run --cpus=3` beside busy loops, which is GitHub's three-core
# runner and what docs/design/conventions.md#reruns says reproduces a flake.
#
#   tools/stress-test.sh                            # the workspace and the JS tests, 20 times
#   ITER=2 CRATE=fennec-core tools/stress-test.sh   # one crate's tests, no JS
#
# Variables: ITER (20), CRATE (empty: --workspace and the JS tests),
# TIMEOUT (seconds for the whole run, 14400), BUSY (busy loops, 3),
# CPUS (3), IMAGE (rust:1-bookworm), LOGDIR (a new directory under /tmp).
#
# Why everything runs in the container: the load is busy loops and many
# loopback clients, and on the host they once exhausted its ports and cut
# every agent off the API. The container's loopback is its own. The busy
# loops are started here and killed by PID on every way out (exit, Ctrl-C,
# a failed test); the container is --init and removed with --rm, and the
# host side kills it by name if interrupted.
#
# Cache: a Docker volume holds the cargo home, the target directory and a
# copy of the sources. The copy is made by content (rsync -c), as
# tools/image-cargo.sh does: cargo takes an output as fresh when its mtime
# is later than its sources', so a bind mount of a worktree whose files are
# older than the last build would compile nothing.
#
# It stops at the first failure, keeps that iteration's log and prints the
# iteration and the failing tests. Exit: 0 all passed, 1 a failure, 2 the
# timeout or a setup error.
set -u

ITER="${ITER:-20}"
CRATE="${CRATE:-}"
TIMEOUT="${TIMEOUT:-14400}"
BUSY="${BUSY:-3}"
CPUS="${CPUS:-3}"
IMAGE="${IMAGE:-rust:1-bookworm}"

if [ "${STRESS_INNER:-}" != 1 ]; then
  root=$(cd "$(dirname "$0")/.." && pwd)
  logdir="${LOGDIR:-$(mktemp -d /tmp/fennec-stress.XXXXXX)}"
  mkdir -p "$logdir"
  name="fennec-stress-$$"
  command -v docker >/dev/null 2>&1 || { echo "stress-test needs docker" >&2; exit 2; }
  # Only this run's container is killed, by the name it was given.
  trap 'docker kill "$name" >/dev/null 2>&1; exit 130' INT TERM
  echo "stress-test: ITER=$ITER CRATE=${CRATE:-workspace} cpus=$CPUS busy=$BUSY logs=$logdir"
  docker run --rm --init --name "$name" --cpus="$CPUS" \
    -v "$root":/src:ro \
    -v fennec-stress-cache:/cache \
    -v "$logdir":/logs \
    -e STRESS_INNER=1 -e ITER="$ITER" -e CRATE="$CRATE" -e TIMEOUT="$TIMEOUT" \
    -e BUSY="$BUSY" \
    "$IMAGE" sh /src/tools/stress-test.sh
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

if ! command -v rsync >/dev/null 2>&1 || { [ -z "$CRATE" ] && ! command -v node >/dev/null 2>&1; }; then
  apt-get update -qq >/dev/null && apt-get install -y -qq --no-install-recommends rsync nodejs make >/dev/null \
    || { echo "stress-test: could not install rsync and node" >&2; exit 2; }
fi
rsync -rc --delete --exclude /target --exclude .git --exclude node_modules /src/ /cache/src/ \
  || { echo "stress-test: could not copy the sources" >&2; exit 2; }
cd /cache/src || exit 2
ln -sfn /cache/target target
rustup show >/dev/null 2>&1

if [ -n "$CRATE" ]; then
  rust_cmd="cargo test -p $CRATE"
else
  rust_cmd="cargo test --workspace --exclude fennec-bench"
  # The JS tests start fennec-server and use the module; build both once.
  cargo build -p fennec-server || exit 2
  make wasm >/dev/null || echo "stress-test: no web/fennec.wasm; the JS tests skip what needs it" >&2
fi

# Build before the load starts, so the iterations measure the tests.
$rust_cmd --no-run >/logs/build.log 2>&1 || { tail -30 /logs/build.log; echo "stress-test: build failed" >&2; exit 2; }

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
