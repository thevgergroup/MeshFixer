#!/usr/bin/env bash
# Hollow benchmarks (MF-ISSUE-112): `meshfixer bench hollow` (one
# mesh.hollow: 2 mm wall, the automatic cell, one auto drain hole) on
# generated UV spheres of radius 50 mm at about 1M, 5M and 30M triangles.
# Each case runs in its own process, one at a time, in the optimised
# build: wall time, average and peak cores, the physical footprint before
# and at its peak, and each stage's time.
#
# Results: artifacts/bench/hollow-<label>.jsonl (one JSON line per case)
# and a Markdown table on stdout. A case that fails or is refused is
# recorded as such, never given a number.
#
# Usage: scripts/bench-hollow.sh [label]   (label defaults to "current")
# Heavy (up to 30M triangles in memory): on demand only, never in the gate.
set -uo pipefail
cd "$(dirname "$0")/.."
root="$(pwd)"
label="${1:-current}"
out="$root/artifacts/bench"
mkdir -p "$out"
results="$out/hollow-$label.jsonl"
: > "$results"

cargo build --release -j "${CARGO_BUILD_JOBS:-4}" -p mf-cli || exit 1
bin="$root/target/release/meshfixer"

# label|args
cases=(
  "sphere 1M|--triangles 1000000"
  "sphere 5M|--triangles 5000000"
  "sphere 30M|--triangles 30000000"
)

for c in "${cases[@]}"; do
  name="${c%%|*}"
  args="${c#*|}"
  echo "== $name" >&2
  # shellcheck disable=SC2086 # the case's arguments are meant to split
  if line="$("$bin" bench hollow $args --label "$name" --json)"; then
    echo "$line" >> "$results"
  else
    echo "{\"model\": \"$name\", \"outcome\": \"failed (exit $?)\"}" >> "$results"
  fi
done

python3 - "$results" <<'PY'
import json, sys
print("| model | triangles | hollow s | avg cores | peak cores | footprint before MB | peak MB | peak above loaded MB | cell | inner faces | outcome |")
print("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|")
for line in open(sys.argv[1]):
    r = json.loads(line)
    if "hollow_s" not in r:
        print(f"| {r['model']} | | | | | | | | | | {r['outcome']} |")
        continue
    print(f"| {r['model']} | {r['triangles']:,} | {r['hollow_s']:.2f} | {r['avg_cores']:.1f} | "
          f"{r['peak_cores']:.1f} | {r['footprint_before_mb']:.0f} | {r['peak_footprint_mb']:.0f} | "
          f"{r['peak_above_loaded_mb']:.0f} | {r['cell_size']} | {r['inner_faces']:,} | {r['outcome']} |")
PY
