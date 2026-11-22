#!/usr/bin/env bash
# Makes the Windows installer's wizard images from the app icon:
#   packaging/windows/wizard-<w>x<h>.bmp  the tall banner on the welcome and
#                                         finish pages (100%, 150%, 200% DPI)
#   packaging/windows/small-<w>x<h>.bmp   the icon in the header of the other pages
# Inno Setup picks the size closest to the display scale. Needs ImageMagick 7
# and macOS's DIN Alternate Bold. Re-run after changing the icon; the BMPs are
# committed so the Windows build doesn't need ImageMagick.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
icon="$root/crates/mf-desktop/assets/icon/meshfixer-1024.png"
out="$root/packaging/windows"
font="/System/Library/Fonts/Supplemental/DIN Alternate Bold.ttf"
[ -f "$font" ] || { echo "missing font: $font" >&2; exit 1; }

# The banner, laid out on a 164-px-wide grid and scaled by $1/164.
banner() {
  local w=$1 h=$2
  local s; s=$(echo "scale=4; $w / 164" | bc)
  px() { printf '%.0f' "$(echo "$1 * $s" | bc)"; }
  magick -size "${w}x${h}" gradient:'#06435A-#021420' \
    \( "$icon" -resize "$(px 124)x$(px 124)" \) -gravity north -geometry "+0+$(px 62)" -composite \
    -font "$font" -fill white -pointsize "$(px 22)" -annotate "+0+$(px 196)" 'MeshFixer' \
    -fill '#8FD3E0' -pointsize "$(px 9.5)" -annotate "+0+$(px 226)" 'Fix, split and print 3D models' \
    -type TrueColor "BMP3:$out/wizard-${w}x${h}.bmp"
}

small() {
  local w=$1 h=$2
  magick "$icon" -resize "${w}x${h}" -background white -gravity center -extent "${w}x${h}" \
    -alpha remove -type TrueColor "BMP3:$out/small-${w}x${h}.bmp"
}

mkdir -p "$out"
banner 164 314; banner 246 459; banner 328 604
small 55 55; small 83 80; small 110 106
ls -l "$out"/*.bmp
