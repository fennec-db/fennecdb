#!/bin/sh
# Puts the fennec-server the app bundles at bin/fennec-server, and the `fennec`
# shell it takes, verifies and restores backups with at bin/fennec, where
# electrobun.config.ts copies them into the app (bin/fennec-server.exe and
# bin/fennec.exe on Windows, run from Git's bash).
#
#   scripts/get-server.sh           build them from this checkout
#                                   (cargo build --release -p fennec-server,
#                                   cargo build --profile cli -p fennec-cli)
#   scripts/get-server.sh 0.1.12    download release v0.1.12's asset for this
#                                   machine, which holds both, and check its
#                                   SHA-256 against the release's own SHA256SUMS
#
# A release's asset is `fennec-v<V>-<target>.tar.gz`, and for Windows
# `fennec-v<V>-<target>.zip` (release.yml's), unpacked by `unzip` or, where
# there is none, by Windows' own tar.exe, which reads a zip.
#
# Either way the server must have the two flags the app starts it with:
# --studio, and --exit-on-stdin-close, by which it stops when the app that holds
# the pipe on its stdin is gone, however it ended. A release older than those
# is refused here, not found out by a window that never opens: the first with
# --studio is 0.1.11, and the first with both is 0.1.12. And the shell must
# have `fennec verify` and --key-file, which the backups use.
#
#   CARGO               the cargo to build with (the Makefile's: ~/.cargo/bin/cargo
#                       where it exists, which has the wasm32 target the repository's
#                       rust-toolchain.toml asks for, else the one on PATH)
#   FENNEC_RELEASES     where releases are downloaded from (a test points it at a
#                       directory with file://), default
#                       https://github.com/fennec-db/fennecdb/releases/download
#   FENNEC_SERVER_OUT   where the server goes, default bin/fennec-server (.exe on Windows)
#   FENNEC_CLI_OUT      where the shell goes, default `fennec` beside the server
#   TARGET_TRIPLE       the release asset's target, default this machine's
set -eu
cd "$(dirname "$0")/.."
root=$(cd ../.. && pwd)

die() {
  echo "get-server: $*" >&2
  exit 1
}

# Git's bash on Windows says MINGW64_NT-<version> (MSYS_NT or CYGWIN_NT in others).
case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*) windows=1 ;;
  *) windows= ;;
esac

version=${1:-}
version=${version#v}

# What this machine's release asset is (a version given), and so what its
# programs are called: a Windows target's end in .exe and come in a zip.
triple=
if [ -n "$version" ]; then
  # The version goes into a URL and a file name: only what a release is called.
  case "$version" in
    *[!0-9A-Za-z.-]* | "" | .* | -*) die "\"$version\" is not a release version (0.1.12, or v0.1.12)" ;;
  esac
  triple=${TARGET_TRIPLE:-}
  if [ -z "$triple" ]; then
    if [ -n "$windows" ]; then
      # A 32-bit or emulated shell reports its own processor; the machine's is in PROCESSOR_ARCHITEW6432 then.
      case "${PROCESSOR_ARCHITEW6432:-${PROCESSOR_ARCHITECTURE:-}}" in
        AMD64) triple=x86_64-pc-windows-msvc ;;
        ARM64) triple=aarch64-pc-windows-msvc ;;
        *) die "no release asset for Windows on ${PROCESSOR_ARCHITECTURE:-an unknown processor}; set TARGET_TRIPLE" ;;
      esac
    else
      case "$(uname -s)-$(uname -m)" in
        Darwin-arm64) triple=aarch64-apple-darwin ;;
        Darwin-x86_64) triple=x86_64-apple-darwin ;;
        Linux-x86_64) triple=x86_64-unknown-linux-musl ;;
        Linux-aarch64 | Linux-arm64) triple=aarch64-unknown-linux-musl ;;
        *) die "no release asset for $(uname -s) $(uname -m); set TARGET_TRIPLE" ;;
      esac
    fi
  fi
fi
case "$triple" in
  *-windows-*) exe=.exe ;;
  "") if [ -n "$windows" ]; then exe=.exe; else exe=; fi ;;
  *) exe= ;;
esac

out=${FENNEC_SERVER_OUT:-bin/fennec-server$exe}
cli_out=${FENNEC_CLI_OUT:-$(dirname "$out")/fennec$exe}
mkdir -p "$(dirname "$out")" "$(dirname "$cli_out")"
# Written beside the targets and moved into place only once both have passed,
# so a refused server or shell never replaces one that works.
stage="$out.new"
cli_stage="$cli_out.new"
rm -f "$stage" "$cli_stage"
tmp=
cleanup() {
  rm -f "$stage" "$cli_stage"
  [ -z "$tmp" ] || rm -rf "$tmp"
}
trap cleanup EXIT

if [ -z "$version" ]; then
  if [ -z "${CARGO:-}" ]; then
    if [ -x "$HOME/.cargo/bin/cargo" ]; then CARGO="$HOME/.cargo/bin/cargo"; else CARGO=cargo; fi
  fi
  # The shell in the profile a release builds it with (panic = abort), as the
  # release's own tarball carries it.
  (cd "$root" && "$CARGO" build --release -p fennec-server && "$CARGO" build --profile cli -p fennec-cli)
  cp "$root/target/release/fennec-server$exe" "$stage"
  cp "$root/target/cli/fennec$exe" "$cli_stage"
else
  case "$triple" in
    *-windows-*) asset=fennec-v$version-$triple.zip ;;
    *) asset=fennec-v$version-$triple.tar.gz ;;
  esac
  base=${FENNEC_RELEASES:-https://github.com/fennec-db/fennecdb/releases/download}/v$version
  tmp=$(mktemp -d "${TMPDIR:-/tmp}/fennec-server.XXXXXX")
  curl -fsSL "$base/SHA256SUMS" -o "$tmp/SHA256SUMS" || die "could not download $base/SHA256SUMS (is there a release v$version?)"
  curl -fsSL "$base/$asset" -o "$tmp/$asset" || die "could not download $base/$asset"
  # `sha256sum`'s lines: "<hash>  <file>". The asset must be listed: a release
  # that does not name it is not one to trust a download of.
  want=$(awk -v f="$asset" '$2 == f { print $1 }' "$tmp/SHA256SUMS")
  [ -n "$want" ] || die "the release's SHA256SUMS does not list $asset"
  if command -v sha256sum >/dev/null 2>&1; then
    got=$(sha256sum "$tmp/$asset" | cut -d' ' -f1)
  else
    got=$(shasum -a 256 "$tmp/$asset" | cut -d' ' -f1)
  fi
  [ "$got" = "$want" ] || die "SHA-256 of $asset is $got, the release says $want: not using it"
  dir="$tmp/fennec-v$version-$triple"
  case "$asset" in
    *.zip)
      # Unpacked whole into the temporary folder, then looked in. Git's bash has
      # no unzip; Windows has tar.exe (bsdtar, which reads a zip) since 2018.
      mkdir "$tmp/unpacked"
      if command -v unzip >/dev/null 2>&1; then
        unzip -q "$tmp/$asset" -d "$tmp/unpacked" || die "$asset is not a zip that unpacks"
      elif [ -n "$windows" ]; then
        # cygpath -W: the Windows folder, as this bash names it (/c/Windows).
        "$(cygpath -W)/System32/tar.exe" -xf "$tmp/$asset" -C "$tmp/unpacked" || die "$asset is not a zip that unpacks"
      else
        die "nothing here unpacks a zip: install unzip"
      fi
      dir="$tmp/unpacked/fennec-v$version-$triple"
      [ -f "$dir/fennec-server$exe" ] || die "$asset holds no fennec-server"
      [ -f "$dir/fennec$exe" ] || die "$asset holds no fennec shell"
      ;;
    *)
      tar -xzf "$tmp/$asset" -C "$tmp" "fennec-v$version-$triple/fennec-server" || die "$asset holds no fennec-server"
      tar -xzf "$tmp/$asset" -C "$tmp" "fennec-v$version-$triple/fennec" || die "$asset holds no fennec shell"
      ;;
  esac
  cp "$dir/fennec-server$exe" "$stage"
  cp "$dir/fennec$exe" "$cli_stage"
  echo "get-server: $asset, SHA-256 $got matches the release's SHA256SUMS"
fi

chmod +x "$stage" "$cli_stage"
help=$("$stage" --help 2>&1 || true)
for flag in --studio --exit-on-stdin-close; do
  printf '%s\n' "$help" | grep -q -- "$flag" ||
    die "this fennec-server has no $flag, which the app starts it with (the first release with --studio is 0.1.11, and with both flags 0.1.12): build the one in this checkout with scripts/get-server.sh, or name a release that has both"
done
cli_help=$("$cli_stage" backup --help 2>&1 || true)
for word in "fennec verify" --key-file; do
  printf '%s\n' "$cli_help" | grep -q -- "$word" ||
    die "this fennec shell has no \`$word\`, which the app's backups use: build the one in this checkout with scripts/get-server.sh, or name a release that has it"
done
mv "$stage" "$out"
mv "$cli_stage" "$cli_out"
ls -l "$out" "$cli_out"
