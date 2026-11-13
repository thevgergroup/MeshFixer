#!/usr/bin/env bash
# Checkpoint C capture (M4): drive the real app headlessly at 1440×900
# (crates/mf-desktop/tests/checkpoint_c.rs, egui_kittest + wgpu), plus a
# CLI-only pass for Part 2's messy-model comparison.
#
# Part 1: the full surgical workflow on the real Archaeopteryx scan
# (fixtures/real/archaeopteryx.obj) — open with the units prompt, inspect,
# select and fill a hole (Smooth then Flat), batch-fill, mark an
# intentional opening, export 3MF. Frames are assembled into
# checkpoint-c.mp4.
#
# Part 2: Smooth vs Flat vs Curvature side by side.
#   - A UV sphere with a large hole: driven through the GUI test
#     (checkpoint_c_part2_fill_styles_sphere).
#   - The messiest real scan with the largest fillable hole (<= 1000
#     edges, the mesh.fill_holes cap): NOT driven through the GUI (the
#     mesh is 637k triangles; two fill+undo round trips in egui_kittest
#     are slow). Instead this script re-derives which messy-* fixture and
#     loop that is by running `meshfixer inspect --json` over every
#     fixtures/real/messy-* file, then calls the release CLI's
#     `fill-holes` (per style) and `render` directly. `render` has no
#     hole-framing option, so these stills show the whole model, not a
#     close-up of the patch (noted in the README).
#
# Part 3: the "remove loose pieces" safeguard on
# fixtures/real/messy-verlan-dress-285440.obj — the Properties preview
# line and the confirmation dialog.
#
# Output: artifacts/m4/checkpoint-c/ (git-ignored).
set -euo pipefail
cd "$(dirname "$0")/.."
root="$(pwd)"
toolchain="$HOME/.rustup/toolchains/$(sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml)-$(rustc -vV | sed -n 's/^host: //p')/bin"
[ -d "$toolchain" ] && export PATH="$toolchain:$PATH"
export CARGO_INCREMENTAL=0
ffmpeg="${FFMPEG:-$(command -v ffmpeg || echo /opt/homebrew/bin/ffmpeg)}"

out="$root/artifacts/m4/checkpoint-c"
rm -rf -- "$out"
mkdir -p "$out"

echo "Largest fillable hole (<= 1000 edges) per fixtures/real/messy-* fixture:"
cargo build --release -j 4 -p mf-cli -p mf-desktop >/dev/null
best_file=""
best_edges=-1
best_loop=""
for f in "$root"/fixtures/real/messy-*; do
  name="$(basename "$f")"
  read -r edges loop_id <<EOF
$("$root/target/release/meshfixer" inspect "$f" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin)
loops = d.get("boundary_loops", [])
under = [l for l in loops if l.get("edge_count", 0) <= 1000]
best = max(under, key=lambda l: l["edge_count"]) if under else None
print((best["edge_count"], best["id"]) if best else (0, -1))
' 2>/dev/null | tr -d '()' | tr ',' ' ' || echo "0 -1")
EOF
  echo "  $name: $edges edges (loop $loop_id)"
  if [ "$edges" -gt "$best_edges" ]; then
    best_edges="$edges"
    best_file="$f"
    best_loop="$loop_id"
  fi
done
best_name="$(basename "$best_file")"
echo "  -> $best_name ($best_edges edges, loop $best_loop) is the largest."

# Part 2's messy-model comparison: the release CLI only, framed on the
# whole model (fill-holes' default --max-loop-edges is 500, well under
# this hole's size, so it's raised explicitly).
work="$(mktemp -d)"
trap 'rm -rf -- "$work"' EXIT
for style in flat smooth; do
  "$root/target/release/meshfixer" fill-holes "$best_file" \
    --select "loop:$best_loop" --max-loop-edges "$best_edges" --fill-style "$style" \
    -o "$work/inopos-$style.obj" >/dev/null
  "$root/target/release/meshfixer" render "$work/inopos-$style.obj" \
    -o "$out/p2-messy-inopos-$style.png" --width 1440 --height 900
done

MF_REQUIRE_GPU=1 MF_REQUIRE_FIXTURES=1 MF_ARTIFACTS="$root/artifacts/m4" \
  cargo test -j 4 -p mf-desktop --test checkpoint_c -- --test-threads=1 --nocapture

if [ -f "$out/frames-part1.txt" ]; then
  grep '^file' "$out/frames-part1.txt" | tail -n 1 >> "$out/frames-part1.txt"
  "$ffmpeg" -loglevel error -y -f concat -safe 0 -i "$out/frames-part1.txt" \
    -vf "fps=30,format=yuv420p" -c:v libx264 -crf 20 -movflags +faststart \
    "$out/checkpoint-c.mp4"
fi
rm -rf -- "$out/frames"
rm -f -- "$out/frames-part1.txt"
echo "wrote $out"
