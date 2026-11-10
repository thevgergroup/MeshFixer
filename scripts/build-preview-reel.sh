#!/usr/bin/env bash
# Build the M4 automated preview reel: an MP4 assembled from real GUI
# frames already captured by the headless tests (checkpoint-a/b/c,
# final-ux). No new frames are rendered here — this only arranges existing
# PNGs under artifacts/m4/ into one video, with a title card and a
# permanent on-screen label, so it can be sent to the owner ahead of the
# live session. It is NOT the human-driven acceptance demo
# (docs/milestones/m4-demo.md) and does not close MF-ISSUE-015.
#
# Uses ImageMagick (`magick`) to scale/pad each frame onto a 1440x900
# canvas and burn in the "automated preview" banner and a caption, then
# ffmpeg only to concatenate and encode (this ffmpeg build has no
# drawtext/libfreetype).
#
# Output: artifacts/m4/demo/m4-preview-reel.mp4 (git-ignored).
# Usage: scripts/build-preview-reel.sh
set -euo pipefail
cd "$(dirname "$0")/.."
root="$(pwd)"
ffmpeg="${FFMPEG:-$(command -v ffmpeg || echo /opt/homebrew/bin/ffmpeg)}"
magick="${MAGICK:-$(command -v magick || echo /opt/homebrew/bin/magick)}"
font="${MF_REEL_FONT:-/System/Library/Fonts/Supplemental/Andale Mono.ttf}"

command -v "$ffmpeg" >/dev/null || { echo "error: ffmpeg not found ($ffmpeg)" >&2; exit 1; }
command -v "$magick" >/dev/null || { echo "error: ImageMagick 'magick' not found ($magick)" >&2; exit 1; }

m4="$root/artifacts/m4"
out_dir="$m4/demo"
mkdir -p "$out_dir"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# Ordered (source, caption) pairs: the M4 story in the order the human demo
# script (docs/milestones/m4-demo.md) tells it — performance at scale,
# surgical repair on the real scan (including Smooth vs Curvature and the
# no-dead-ends UX fixes), then multi-object. Every file must exist; a
# missing one is a hard error, since this only rearranges existing renders.
frames=(
  "$m4/checkpoint-a/01-two-transformed-objects.png|1. Multi-object scene: two objects at their own 3MF transforms"
  "$m4/checkpoint-b/b-5m-01-defect-overlays.png|2. Performance: 5M-triangle damaged sphere, defects overlaid"
  "$m4/checkpoint-b/b-5m-hover-02-hole.png|3. 5M scale: hovering a hole stays responsive"
  "$m4/checkpoint-c/01-units-prompt-metres-preselected.png|4. Real Archaeopteryx scan: unit prompt preselects metres"
  "$m4/checkpoint-c/02-objects-and-defects-size-groups.png|5. Defects grouped by size, with counts"
  "$m4/checkpoint-c/04-hole-selected-properties-fill-smooth.png|6. A hole selected: Properties offers Fill, Smooth preselected"
  "$m4/checkpoint-c/05-filled-smooth-recent-repair-tint.png|7. Filled (Smooth): the new patch tinted as a recent repair"
  "$m4/checkpoint-c/06-undo-back-to-open-hole.png|8. Undo: back to the open hole"
  "$m4/final-ux/i1-curvature-before-apply.png|9. Curvature-matched warns on large holes before Apply"
  "$m4/checkpoint-c/p2-sphere-11-smooth.png|10. Large hole, Smooth fill: matches the surrounding curvature"
  "$m4/checkpoint-c/p2-sphere-11-curvature.png|11. Same hole, Curvature-matched: less accurate above 60 edges"
  "$m4/checkpoint-c/07-marked-intentional-opening.png|12. A base opening marked as an intentional opening"
  "$m4/final-ux/c1-very-large-hole.png|13. Very large holes get a plain verdict and a next action, not a dead end"
  "$m4/final-ux/c1-tangled-edge.png|14. Tangled edges: what it means for printing, and Select its faces"
  "$m4/checkpoint-c/08-batch-fill-preview.png|15. Batch fill preview across many holes"
  "$m4/checkpoint-c/09-batch-fill-applied-opening-still-marked.png|16. Batch fill applied; the marked opening stays open"
  "$m4/final-ux/i3-intentional-opening.png|17. Intentional openings get their own colour, not hole-orange"
  "$m4/final-ux/i2-import-warnings.png|18. Import warnings are readable, not just counted"
  "$m4/checkpoint-c/10-exported-3mf-status-line.png|19. Export to 3MF confirmed in the status line"
  "$m4/checkpoint-a/15-piece-removed.png|20. A loose piece removed, with its own undo step"
  "$m4/final-ux/i5-1280x720-panels.png|21. Laptop sizes: panels collapse, the view stays dominant"
)

for entry in "${frames[@]}"; do
  src="${entry%%|*}"
  [ -f "$src" ] || { echo "error: missing frame $src (re-run the capture scripts first)" >&2; exit 1; }
done

seconds_per_frame=2.5
fps=30
banner="AUTOMATED PREVIEW -- NOT THE HUMAN DEMO"

list="$work/frames.txt"
: > "$list"
i=0
for entry in "${frames[@]}"; do
  src="${entry%%|*}"
  caption="${entry#*|}"
  i=$((i + 1))
  padded=$(printf "%03d" "$i")
  dst="$work/$padded.png"
  "$magick" "$src" \
    -resize 1440x900 -gravity center -background '#1a1a1a' -extent 1440x900 \
    -font "$font" \
    -gravity North -fill white -undercolor '#b33a3aee' -pointsize 20 -annotate +0+10 "$banner" \
    -gravity SouthWest -fill white -undercolor '#000000cc' -pointsize 22 -annotate +16+16 "$caption" \
    "$dst"
  printf "file '%s'\nduration %s\n" "$dst" "$seconds_per_frame" >> "$list"
done
# The concat demuxer needs the last file repeated (its "duration" line is
# otherwise ignored). Captured before appending, so this never reads and
# writes "$list" in the same pipeline.
last_file_line="$(grep '^file' "$list" | tail -n 1)"
printf '%s\n' "$last_file_line" >> "$list"

out="$out_dir/m4-preview-reel.mp4"
"$ffmpeg" -loglevel error -y -f concat -safe 0 -i "$list" \
  -vf "fps=$fps,format=yuv420p" -c:v libx264 -crf 20 -movflags +faststart \
  "$out"

echo "wrote $out (${#frames[@]} frames, label: $banner)"
