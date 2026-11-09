#!/usr/bin/env bash
# Checkpoint B follow-up capture (M4 Task 8a): drive the real app
# headlessly at 1440×900 (crates/mf-desktop/tests/checkpoint_b_followup.rs,
# egui_kittest + wgpu) on the real Archaeopteryx scan (fixtures/real,
# fetched by scripts/fetch-fixtures.sh): double-click a tiny loose piece,
# a far-side hole and the object's row in the side panel. Keeps the stills
# and assembles the frames into an MP4 with ffmpeg. Output:
# artifacts/m4/checkpoint-b-followup/ (git-ignored).
set -euo pipefail
cd "$(dirname "$0")/.."
root="$(pwd)"
toolchain="$HOME/.rustup/toolchains/$(sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml)-$(rustc -vV | sed -n 's/^host: //p')/bin"
[ -d "$toolchain" ] && export PATH="$toolchain:$PATH"
export CARGO_INCREMENTAL=0
ffmpeg="${FFMPEG:-$(command -v ffmpeg || echo /opt/homebrew/bin/ffmpeg)}"

out="$root/artifacts/m4/checkpoint-b-followup"
rm -rf -- "$out"
MF_REQUIRE_GPU=1 MF_REQUIRE_FIXTURES=1 MF_ARTIFACTS="$root/artifacts/m4" \
  cargo test -p mf-desktop --test checkpoint_b_followup -- --test-threads=1 --nocapture
# The last file is repeated so the concat demuxer honours its duration.
grep '^file' "$out/frames.txt" | tail -n 1 >> "$out/frames.txt"
"$ffmpeg" -loglevel error -y -f concat -safe 0 -i "$out/frames.txt" \
  -vf "fps=30,format=yuv420p" -c:v libx264 -crf 20 -movflags +faststart \
  "$out/checkpoint-b-followup.mp4"
rm -rf -- "$out/frames"
rm -f -- "$out/frames.txt"
echo "wrote $out"
