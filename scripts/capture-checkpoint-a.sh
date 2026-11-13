#!/usr/bin/env bash
# Checkpoint A capture: drive the real app headlessly at 1440×900
# (crates/mf-desktop/tests/checkpoint_a.rs, egui_kittest + wgpu), keep its
# stills, and assemble the frames into an MP4 with ffmpeg. Output:
# artifacts/m4/checkpoint-a/ (git-ignored).
set -euo pipefail
cd "$(dirname "$0")/.."
root="$(pwd)"
toolchain="$HOME/.rustup/toolchains/$(sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml)-$(rustc -vV | sed -n 's/^host: //p')/bin"
[ -d "$toolchain" ] && export PATH="$toolchain:$PATH"
export CARGO_INCREMENTAL=0
ffmpeg="${FFMPEG:-$(command -v ffmpeg || echo /opt/homebrew/bin/ffmpeg)}"

out="$root/artifacts/m4/checkpoint-a"
rm -rf -- "$out"
MF_REQUIRE_GPU=1 MF_ARTIFACTS="$root/artifacts/m4" \
  cargo test -p mf-desktop --test checkpoint_a -- --nocapture
"$ffmpeg" -loglevel error -y -f concat -safe 0 -i "$out/frames.txt" \
  -vf "fps=30,format=yuv420p" -c:v libx264 -crf 20 -movflags +faststart \
  "$out/checkpoint-a.mp4"
rm -rf -- "$out/frames" "$out/frames.txt"
echo "wrote $out"
