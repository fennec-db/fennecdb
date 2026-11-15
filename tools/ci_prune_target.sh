#!/bin/sh
# What a CI job's cargo cache keeps of target/: the compiled crates from
# outside the workspace, and nothing built from this checkout. A checkout's
# files carry new times, so cargo builds the workspace's own crates again
# whatever target/ holds (ci.yml's postgres job says so); what a restored
# target/ saves is the registry's crates (rustls, ring, ...), while the
# workspace's test binaries and debug info were most of every entry's bytes.
# ci.yml runs this on main only, just before it saves the cache.
#
#   sh tools/ci_prune_target.sh [target-dir]    (default: target)
#
# A unit is the workspace's when its dep-info (`deps/*.d`, the sources it was
# built from) names no file under the registry or a git checkout; its outputs
# are every `deps/` file of its stem (found, not globbed: a test binary
# leaves thousands of object files), and its fingerprint goes with them.
set -eu
t=${1:-target}
[ -d "$t" ] || exit 0
before=$(du -sk "$t" | cut -f1)

for deps in "$t"/*/deps "$t"/*/*/deps; do
  [ -d "$deps" ] || continue
  for d in "$deps"/*.d; do
    [ -f "$d" ] || continue
    grep -Eq 'registry[/\\]+src|git[/\\]+checkouts' "$d" && continue
    stem=$(basename "$d" .d)
    find "$deps" -maxdepth 1 \( -name "$stem" -o -name "$stem.*" -o -name "lib$stem.*" \) -exec rm -f {} +
  done
done

for profile in "$t"/*/ "$t"/*/*/; do
  [ -d "$profile" ] || continue
  rm -rf "$profile"incremental "$profile"examples
  rm -rf "$profile".fingerprint/fennec-* "$profile"build/fennec-*
  # The top-level copies of the workspace's binaries and libraries.
  find "$profile" -maxdepth 1 -type f \( -name 'fennec*' -o -name 'libfennec*' \) -exec rm -f {} +
done

after=$(du -sk "$t" | cut -f1)
echo "target/ pruned: $((before / 1024)) MiB -> $((after / 1024)) MiB"
