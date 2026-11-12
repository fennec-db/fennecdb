#!/bin/sh
# icon/icon.svg -> icon.iconset/, which Electrobun turns into the .icns
# (build.mac.icons), with the tools macOS has: sips draws the SVG and
# resizes, nothing is downloaded.
#
# `make-icon.sh rasters` makes, from the same render, what Windows and Linux
# are given instead (build.win.icon, build.linux.icon, and what a .fennec
# file's registration shows: src/bun/associate.ts): icon/icon.ico (16 to 256
# pixels, each a PNG) and icon/icon.png (512), with icon/rasters.json naming
# the SVG they were made from. Those are kept in git, since neither system has
# an SVG renderer of its own to make them where they are built; after a change
# to the SVG, run this on a Mac and commit the three (test/icon.test.ts fails
# until then).
set -eu
cd "$(dirname "$0")/.."

svg=icon/icon.svg
set_dir=icon.iconset
big=$(mktemp -d "${TMPDIR:-/tmp}/fennec-icon.XXXXXX")
trap 'rm -rf "$big"' EXIT

# One 1024-point render, every other size is cut from it. sips keeps the alpha
# channel, which the squircle's corners need. An older macOS's sips does not
# read SVG; QuickLook's thumbnailer, which is part of the system too, does.
if ! sips -s format png "$svg" --out "$big/1024.png" >/dev/null 2>&1; then
  echo "make-icon: sips cannot draw $svg here, using qlmanage" >&2
  qlmanage -t -s 1024 -o "$big" "$svg" >/dev/null 2>&1 || true
  [ -f "$big/$(basename "$svg").png" ] || {
    echo "make-icon: neither sips nor qlmanage could draw $svg" >&2
    exit 1
  }
  mv "$big/$(basename "$svg").png" "$big/1024.png"
fi

if [ "${1:-}" = rasters ]; then
  sizes="16 24 32 48 64 128 256"
  for size in $sizes; do
    sips -z "$size" "$size" "$big/1024.png" --out "$big/ico-$size.png" >/dev/null
  done
  sips -z 512 512 "$big/1024.png" --out icon/icon.png >/dev/null
  # shellcheck disable=SC2086 # the sizes, one word each
  node scripts/make-ico.mjs icon/icon.ico $(for size in $sizes; do printf '%s ' "$big/ico-$size.png"; done)
  node scripts/make-ico.mjs --record icon
  echo "icon/icon.ico ($(wc -c < icon/icon.ico | tr -d ' ') bytes, $sizes) and icon/icon.png (512, $(wc -c < icon/icon.png | tr -d ' ') bytes) from $svg"
  exit 0
fi

rm -rf "$set_dir"
mkdir -p "$set_dir"
# A point size and its @2x, as iconutil names them.
for size in 16 32 128 256 512; do
  double=$((size * 2))
  sips -z "$size" "$size" "$big/1024.png" --out "$set_dir/icon_${size}x${size}.png" >/dev/null
  sips -z "$double" "$double" "$big/1024.png" --out "$set_dir/icon_${size}x${size}@2x.png" >/dev/null
done
# iconutil would refuse a set it cannot read; this is the cheapest way to say
# so here and not in the middle of a build.
iconutil -c icns "$set_dir" -o "$big/check.icns"
echo "icon.iconset: $(ls "$set_dir" | wc -l | tr -d ' ') images from $svg"
