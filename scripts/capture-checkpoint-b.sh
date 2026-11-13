#!/usr/bin/env bash
# Checkpoint B capture: drive the real app headlessly at 1440×900
# (crates/mf-desktop/tests/checkpoint_b.rs, egui_kittest + wgpu) on the
# real Archaeopteryx scan (fixtures/real, fetched by
# scripts/fetch-fixtures.sh) and on a 5M-triangle damaged mesh generated
# in memory; keep the stills and assemble the frames into an MP4 with
# ffmpeg. Output: artifacts/m4/checkpoint-b/ (git-ignored).
set -euo pipefail
cd "$(dirname "$0")/.."
root="$(pwd)"
toolchain="$HOME/.rustup/toolchains/$(sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml)-$(rustc -vV | sed -n 's/^host: //p')/bin"
[ -d "$toolchain" ] && export PATH="$toolchain:$PATH"
export CARGO_INCREMENTAL=0
ffmpeg="${FFMPEG:-$(command -v ffmpeg || echo /opt/homebrew/bin/ffmpeg)}"

out="$root/artifacts/m4/checkpoint-b"
rm -rf -- "$out"
MF_REQUIRE_GPU=1 MF_REQUIRE_FIXTURES=1 MF_ARTIFACTS="$root/artifacts/m4" \
  cargo test -p mf-desktop --test checkpoint_b -- --test-threads=1 --nocapture
# The scan's walkthrough, then the 5M one; the last file is repeated so
# the concat demuxer honours its duration.
cat "$out/frames-a-scan.txt" "$out/frames-b-5m.txt" > "$out/frames.txt"
grep '^file' "$out/frames.txt" | tail -n 1 >> "$out/frames.txt"
"$ffmpeg" -loglevel error -y -f concat -safe 0 -i "$out/frames.txt" \
  -vf "fps=30,format=yuv420p" -c:v libx264 -crf 20 -movflags +faststart \
  "$out/checkpoint-b.mp4"
rm -rf -- "$out/frames"
rm -f -- "$out/frames.txt" "$out/frames-a-scan.txt" "$out/frames-b-5m.txt"
echo "wrote $out"
