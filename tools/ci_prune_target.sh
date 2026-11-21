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
# Cargo 1.100 keeps each unit in a directory of its own,
# `<profile>/build/<package>/<hash>/`: its outputs and dep-info in `out/`,
# its fingerprint in `fingerprint/`, a build script's run in `run/` (and
# what the script made, ring's C objects, in that unit's `out/`). A package
# is the workspace's when none of its units' dep-info (`out/*.d`, the
# sources a unit was built from) names a file under the registry or a git
# checkout: the workspace's packages go whole, every unit and hash of them,
# and a registry crate's stay whole, its build script's run among them,
# since rebuilding one is what the cache saves. What Cargo before 1.100
# kept -- `deps/`, `.fingerprint/`, `build/<package>-<hash>/` -- no build
# reads now, and goes too: an entry restored across a bump carried a dead
# generation of it on into every save (the keys restore within one
# toolchain since).
set -eu
t=${1:-target}
[ -d "$t" ] || exit 0
before=$(du -sk "$t" | cut -f1)

for profile in "$t"/*/ "$t"/*/*/; do
  [ -d "$profile" ] || continue
  if [ -d "$profile"build ]; then
    for package in "$profile"build/*/; do
      [ -d "$package" ] || continue
      kept=
      for d in "$package"*/out/*.d; do
        [ -f "$d" ] || continue
        if grep -Eq 'registry[/\\]+src|git[/\\]+checkouts' "$d"; then
          kept=1
          break
        fi
      done
      [ -n "$kept" ] || rm -rf "$package"
    done
  fi
  rm -rf "$profile"incremental "$profile"examples "$profile"deps "$profile".fingerprint
  # The top-level copies of the workspace's binaries and libraries.
  find "$profile" -maxdepth 1 -type f \( -name 'fennec*' -o -name 'libfennec*' \) -exec rm -f {} +
done

after=$(du -sk "$t" | cut -f1)
echo "target/ pruned: $((before / 1024)) MiB -> $((after / 1024)) MiB"
