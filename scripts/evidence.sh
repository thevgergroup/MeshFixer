#!/usr/bin/env bash
# Produce milestone evidence: repaired sample model, diagnostics before and
# after, renders, and timing/memory numbers with machine info.
# Usage: scripts/evidence.sh <out-dir>   (expects a release build of mf-cli)
set -euo pipefail
out="$1"
mkdir -p "$out"
bin=target/release/meshfixer
time_cmd() { # label, command... (fails, with the command's output, if it fails)
  local label="$1"; shift
  if [ "$(uname)" = "Darwin" ]; then
    /usr/bin/time -l "$@" > /dev/null 2> "$out/time-$label.txt" || { cat "$out/time-$label.txt" >&2; return 1; }
    rss=$(awk '/maximum resident/ {printf "%.0f", $1/1048576}' "$out/time-$label.txt")
    wall=$(awk '/ real / {print $1}' "$out/time-$label.txt")
  else
    /usr/bin/time -v "$@" > /dev/null 2> "$out/time-$label.txt" || { cat "$out/time-$label.txt" >&2; return 1; }
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
# Allocator-level memory breakdown (MF-ISSUE-013): phases plus retained structures.
$bin inspect "$out/damaged-1m.stl" --json --metrics "$out/metrics-inspect-1m.json" > /dev/null
$bin remove-small-components "$out/damaged-1m.stl" -o "$out/tmp-metrics.stl" --metrics "$out/metrics-remove-small-1m.json"
rm -f "$out/tmp-metrics.stl"
# The working set at 5M requested (≈4.8M actual) triangles (mesh + cached topology) and topology build peak
# (MF-GEO-002, MF-ISSUE-019); CI's memory-gate job fails it above 400 MB.
$bin bench memory --triangles 5000000 --json > "$out/memory-5m.json"
# Command log + verified replay (MF-CMD-004).
$bin run "$out/damaged-1m.stl" --op mesh.remove_small_components --param max_triangles=100 \
  --save-log "$out/session.mfx.log" -o "$out/tmp-run.stl"
$bin replay "$out/session.mfx.log" > "$out/replay-report.txt"
rm -f "$out/tmp-run.stl"
$bin render "$out/damaged-1m.stl" -o "$out/before.png"
$bin render "$out/repaired-1m.stl" -o "$out/after.png"
# M3 surgical repair: remove islands, fill every hole (refined), export 3MF;
# the replay covers fill_holes, whose refinement computes new positions.
$bin run "$out/damaged-1m.stl" --op mesh.remove_small_components --param max_triangles=100 \
  --op mesh.fill_holes --save-log "$out/fill-session.mfx.log" -o "$out/filled-1m.3mf"
$bin inspect "$out/filled-1m.3mf" --json > "$out/inspect-filled.json"
$bin replay "$out/fill-session.mfx.log" > "$out/replay-report-fill.txt"
$bin render "$out/filled-1m.3mf" -o "$out/filled.png"

# Each run is assigned first so a failing benchmark stops the script
# (set -e) instead of publishing a bogus number.
r_inspect=$(time_cmd inspect-1m "$bin" inspect "$out/damaged-1m.stl" --json)
r_remove=$(time_cmd remove-small-1m "$bin" remove-small-components "$out/damaged-1m.stl" -o "$out/tmp.stl")
r_fill=$(time_cmd fill-holes-1m "$bin" fill-holes "$out/repaired-1m.stl" -o "$out/tmp.stl")
r_fill_3mf=$(time_cmd fill-holes-export-3mf-1m "$bin" fill-holes "$out/repaired-1m.stl" -o "$out/tmp.3mf")
{
  echo "{"
  echo "  \"machine\": {\"os\": \"$(uname -srm)\", \"cpus\": $(getconf _NPROCESSORS_ONLN), \"runner\": \"${RUNNER_NAME:-local}\"},"
  echo "  \"runs\": ["
  echo "    $r_inspect,"
  echo "    $r_remove,"
  echo "    $r_fill,"
  echo "    $r_fill_3mf"
  echo "  ]"
  echo "}"
} > "$out/benchmarks.json"
rm -f "$out/tmp.stl" "$out/tmp.3mf"
cat "$out/benchmarks.json"
