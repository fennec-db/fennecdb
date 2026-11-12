#!/bin/sh
# Attaches the app's downloads to a published release and appends each one's
# line to the release's SHA256SUMS, which only grows: desktop.yml's
# attach-unsigned, a job with contents: write and no secret, runs it from main
# with what the build jobs uploaded, and test/attach.test.ts runs it against a
# stand-in `gh` that keeps a release in a folder.
#
#   scripts/attach.sh vX.Y.Z FILE...
#
# FILE is a download as a build job names it, each attached under the
# release's version:
#
#   fennec-studio-macos-arm64.dmg     -> fennec-studio-vX.Y.Z-macos-arm64.dmg
#   fennec-studio-windows-x64.zip     -> fennec-studio-vX.Y.Z-windows-x64.zip
#   fennec-studio-windows-arm64.zip   -> ...
#   fennec-studio-linux-x64.tar.xz
#   fennec-studio-linux-arm64.tar.xz
#
# SHA256SUMS is the one place install.sh --app and install.ps1 -App learn a
# sum from, and every sum anyone took from it stays: the new file starts
# with every byte of the old (`cmp -n`), has one line more for each file it
# lists anew, and reads back as written. A file of an asset's name already on
# the release is never replaced: one whose SHA-256 is this file's is a
# re-run's and is gone on with, any other fails the run -- before anything
# is written, every file is held to what the release has. GitHub cannot
# write an asset in place, so `--clobber` deletes SHA256SUMS and uploads it,
# and for that moment the release has none: one write for all the files.
#
# GH_TOKEN and GITHUB_REPOSITORY as a workflow sets them; `gh` is the one
# program that reaches GitHub, and nothing downloaded is run.
set -eu

die() {
  echo "::error::$*" >&2
  exit 1
}

release=${1:-}
[ "$#" -gt 0 ] && shift
printf '%s' "$release" | grep -Eq '^v?[0-9]+\.[0-9]+\.[0-9]+$' || die "release is a tag such as v0.2.0, not '$release'"
version=${release#v}
tag=v$version
[ "$#" -gt 0 ] || die "nothing to attach to $tag: the jobs that make the downloads failed or did not run"
repo=${GITHUB_REPOSITORY:?GITHUB_REPOSITORY names the repository}

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

work=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/attach.XXXXXX")
trap 'rm -rf "$work"' EXIT
mkdir "$work/release" "$work/readback" "$work/out"

have=$(gh release view "$tag" -R "$repo" --json assets -q '.assets[].name')
printf '%s\n' "$have" | grep -qxF SHA256SUMS || die "release $tag has no SHA256SUMS to list the app in"
gh release download "$tag" -R "$repo" -p SHA256SUMS -D "$work/release"
sums=$work/release/SHA256SUMS

# Every file held to the release before anything is written: named as a
# download is, its asset there already this file or not there at all, its
# line there already with this sum or not there at all.
upload=
lines=
for file in "$@"; do
  base=$(basename "$file")
  case $base in
    fennec-studio-macos-arm64.dmg | fennec-studio-windows-x64.zip | fennec-studio-windows-arm64.zip | fennec-studio-linux-x64.tar.xz | fennec-studio-linux-arm64.tar.xz) ;;
    *) die "$base is not one of the app's downloads" ;;
  esac
  [ -f "$file" ] || die "no $file"
  name=fennec-studio-v$version-${base#fennec-studio-}
  [ ! -e "$work/out/$name" ] || die "$base is named twice"
  cp "$file" "$work/out/$name"
  sum=$(sha256 "$work/out/$name")
  echo "$name: $(wc -c <"$work/out/$name" | tr -d ' ') bytes, SHA-256 $sum"
  if printf '%s\n' "$have" | grep -qxF "$name"; then
    gh release download "$tag" -R "$repo" -p "$name" -D "$work/release"
    [ "$(sha256 "$work/release/$name")" = "$sum" ] || die "release $tag has a $name already, not this one: a release's assets are never replaced"
    echo "  on the release already, this one"
  else
    upload="$upload $name"
  fi
  listed=$(awk -v n="$name" '{ f = $2; sub(/^\*/, "", f); if (f == n) print $1 }' "$sums")
  if [ -n "$listed" ]; then
    [ "$listed" = "$sum" ] || die "SHA256SUMS lists $name with $listed, not $sum"
    echo "  listed in SHA256SUMS already"
  else
    lines="$lines$sum  $name
"
  fi
done

for name in $upload; do
  gh release upload "$tag" "$work/out/$name" -R "$repo"
done

if [ -z "$lines" ]; then
  echo "SHA256SUMS of $tag lists every one already"
  exit 0
fi
cp "$sums" "$work/out/SHA256SUMS"
[ -z "$(tail -c 1 "$work/out/SHA256SUMS")" ] || echo >>"$work/out/SHA256SUMS"
printf '%s' "$lines" >>"$work/out/SHA256SUMS"
# Its first bytes are the old file's, as many as it had (`head -c`, not
# `cmp -n`, which macOS's cmp fails at the shorter file's end).
old=$(wc -c <"$sums" | tr -d ' ')
head -c "$old" "$work/out/SHA256SUMS" | cmp -s - "$sums" || die "the new SHA256SUMS does not start with the old"
added=$(printf '%s' "$lines" | grep -c .)
[ "$(grep -c . "$work/out/SHA256SUMS")" -eq "$(($(grep -c . "$sums") + added))" ] || die "the new SHA256SUMS is not the old with $added lines more"
gh release upload "$tag" "$work/out/SHA256SUMS" --clobber -R "$repo"
gh release download "$tag" -R "$repo" -p SHA256SUMS -D "$work/readback"
cmp "$work/out/SHA256SUMS" "$work/readback/SHA256SUMS" || die "SHA256SUMS of $tag does not read back as written"
echo "SHA256SUMS of $tag now ends with:"
tail -n "$added" "$work/readback/SHA256SUMS"
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  {
    echo "### Attached to $tag"
    echo
    printf '%s' "$lines" | while read -r sum name; do
      echo "- \`$name\`, $(wc -c <"$work/out/$name" | tr -d ' ') bytes, SHA-256 \`$sum\`"
    done
    echo
    echo "Each listed in the release's SHA256SUMS."
  } >>"$GITHUB_STEP_SUMMARY"
fi
