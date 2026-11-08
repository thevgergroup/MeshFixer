#!/usr/bin/env bash
# Produce milestone evidence: repaired sample model, diagnostics before and
# after, renders, and timing/memory numbers with machine info.
# Usage: scripts/evidence.sh <out-dir>   (expects a release build of mf-cli)
set -euo pipefail
out="$1"
mkdir -p "$out"
bin=target/release/meshfixer
time_cmd() { # label, command...
  local label="$1"; shift
  if [ "$(uname)" = "Darwin" ]; then
    /usr/bin/time -l "$@" > /dev/null 2> "$out/time-$label.txt"
    rss=$(awk '/maximum resident/ {printf "%.0f", $1/1048576}' "$out/time-$label.txt")
    wall=$(awk '/ real / {print $1}' "$out/time-$label.txt")
  else
    /usr/bin/time -v "$@" > /dev/null 2> "$out/time-$label.txt"
    rss=$(awk -F: '/Maximum resident/ {printf "%.0f", $2/1024}' "$out/time-$label.txt")
    # "Elapsed (wall clock) time (h:mm:ss or m:ss): 0:00.55"
    wall=$(awk '/Elapsed \(wall clock\)/ {n=split($NF,p,":"); s=0; for(i=1;i<=n;i++) s=s*60+p[i]; print s}' "$out/time-$label.txt")
  fi
  echo "{\"label\":\"$label\",\"wall_s\":$wall,\"peak_rss_mb\":$rss}"
}

$bin gen-fixture damaged "$out/damaged-1m.stl" --triangles 1000000 --seed 1
$bin inspect "$out/damaged-1m.stl" --json > "$out/inspect-before.json"
$bin remove-small-components "$out/damaged-1m.stl" -o "$out/repaired-1m.stl" --max-triangles 100
$bin inspect "$out/repaired-1m.stl" --json > "$out/inspect-after.json"
$bin render "$out/damaged-1m.stl" -o "$out/before.png"
$bin render "$out/repaired-1m.stl" -o "$out/after.png"

{
  echo "{"
  echo "  \"machine\": {\"os\": \"$(uname -srm)\", \"cpus\": $(getconf _NPROCESSORS_ONLN), \"runner\": \"${RUNNER_NAME:-local}\"},"
  echo "  \"runs\": ["
  echo "    $(time_cmd inspect-1m "$bin" inspect "$out/damaged-1m.stl" --json),"
  echo "    $(time_cmd remove-small-1m "$bin" remove-small-components "$out/damaged-1m.stl" -o "$out/tmp.stl")"
  echo "  ]"
  echo "}"
} > "$out/benchmarks.json"
rm -f "$out/tmp.stl" "$out/damaged-1m.stl"
cat "$out/benchmarks.json"
