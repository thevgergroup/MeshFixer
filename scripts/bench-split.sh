#!/usr/bin/env bash
# Split benchmarks (owner directive 2026-11-13, split-manifold-brief.md):
# `meshfixer bench split` (one mesh.split_plane, keep both, capped, a
# tilted plane through the model's centre) at about 1M, 5M and 30M
# triangles, on generated UV spheres and on real scans from fixtures/real
# (closed ones; subdivided where that is what reaches the size). Each case
# runs in its own process, one at a time, in the optimised build: wall
# time, average and peak cores, and the physical footprint before and at
# its peak during the split.
#
# Results: artifacts/bench/split-<label>.jsonl (one JSON line per case)
# and a Markdown table on stdout. A case that fails or is refused is
# recorded as such, never given a number.
#
# Usage: scripts/bench-split.sh [label]   (label defaults to "current")
# Heavy (up to 30M triangles in memory): on demand only, never in the gate.
set -uo pipefail
cd "$(dirname "$0")/.."
root="$(pwd)"
label="${1:-current}"
out="$root/artifacts/bench"
mkdir -p "$out"
results="$out/split-$label.jsonl"
: > "$results"

cargo build --release -j "${CARGO_BUILD_JOBS:-4}" -p mf-cli || exit 1
bin="$root/target/release/meshfixer"
real="$root/fixtures/real"

# label|args
cases=(
  "sphere 1M|--triangles 1000000"
  "sphere 5M|--triangles 5000000"
  "sphere 30M|--triangles 30000000"
  "sullivan scan x4 (1.6M)|$real/messy-sullivan-1716279.stl --subdivide 1"
  "winged-victory scan (5.7M)|$real/messy-winged-victory-376253.obj"
  "winged-victory scan x4 (22.7M)|$real/messy-winged-victory-376253.obj --subdivide 1"
)

for c in "${cases[@]}"; do
  name="${c%%|*}"
  args="${c#*|}"
  echo "== $name" >&2
  # shellcheck disable=SC2086 # the case's arguments are meant to split
  if line="$("$bin" bench split $args --label "$name" --json)"; then
    echo "$line" >> "$results"
  else
    echo "{\"model\": \"$name\", \"outcome\": \"failed (exit $?)\"}" >> "$results"
  fi
done

python3 - "$results" <<'EOF'
import json, sys
print("| model | triangles | split s | avg cores | peak cores | footprint before MB | peak MB | peak above loaded MB | outcome |")
print("|---|---:|---:|---:|---:|---:|---:|---:|---|")
for line in open(sys.argv[1]):
    r = json.loads(line)
    if "split_s" not in r:
        print(f"| {r['model']} | | | | | | | | {r['outcome']} |")
        continue
    print(f"| {r['model']} | {r['triangles']:,} | {r['split_s']:.2f} | {r['avg_cores']:.1f} | "
          f"{r['peak_cores']:.1f} | {r['footprint_before_mb']:.0f} | {r['peak_footprint_mb']:.0f} | "
          f"{r['peak_above_loaded_mb']:.0f} | {r['outcome']} |")
EOF
