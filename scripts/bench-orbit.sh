#!/usr/bin/env bash
# Orbit benchmarks (MF-RENDER-002, MF-RT-005): `meshfixer bench orbit` on
# the damaged fixture at 1M, 5M and 10M requested triangles (the fixture's
# holes leave slightly fewer: each result says "requested (actual)"), plus
# 30M as information only. Each fixture is generated as a file in a fresh
# `mktemp -d` directory, benchmarked from there, and deleted (by absolute
# path) before the next size, so only one is ever on disk or in memory.
#
# Runs one size at a time. A size is skipped, with the reason recorded,
# when its estimated memory need exceeds what's available (checked before
# its fixture is generated: `--only-check-memory`, and again before the
# import: `--skip-if-short-of-memory`); a size that fails is recorded as
# failed. Neither is ever given a number.
#
# Results: artifacts/m4/bench/orbit-<size>.json (and .log), plus
# orbit-summary.md. Usage: scripts/bench-orbit.sh [label] [frames] [WxH]
# (label defaults to "preliminary"; frames 600; 1920x1080).
set -uo pipefail
cd "$(dirname "$0")/.."
root="$(pwd)"
label="${1:-preliminary}"
frames="${2:-600}"
size="${3:-1920x1080}"
out="$root/artifacts/m4/bench"
mkdir -p "$out"

cargo build --release -p mf-cli || exit 1
bin="$root/target/release/meshfixer"

run_size() {
  local name="$1" triangles="$2"
  local json="$out/orbit-$name.json" log="$out/orbit-$name.log"
  echo "=== $name: $triangles requested triangles ($frames frames at $size) ==="
  : >"$log"
  # Is there room? Decided before generating anything.
  if ! "$bin" bench orbit --triangles "$triangles" --frames "$frames" --size "$size" \
    --only-check-memory --json >"$json" 2>>"$log"; then
    echo "$name: the memory check failed; see $log"
    printf '{"failed": true, "exit_status": 1, "requested_triangles": %d, "log": "orbit-%s.log"}\n' \
      "$triangles" "$name" >"$json"
    return 0
  fi
  if grep -q '"skipped": true' "$json"; then
    echo "$name: skipped: $(sed -n 's/.*"reason": "\(.*\)".*/\1/p' "$json")"
    return 0
  fi
  local tmp status=0
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/mf-bench-orbit.XXXXXX")" || return 1
  "$bin" gen-fixture damaged "$tmp/damaged-$name.stl" --triangles "$triangles" --seed 11 \
    >>"$log" 2>&1 || status=$?
  if [ "$status" -eq 0 ]; then
    "$bin" bench orbit "$tmp/damaged-$name.stl" --triangles "$triangles" --frames "$frames" \
      --size "$size" --skip-if-short-of-memory --json --metrics "$out/orbit-$name.metrics.json" \
      >"$json" 2>>"$log" || status=$?
  fi
  rm -rf "$tmp"
  if [ "$status" -ne 0 ]; then
    echo "$name failed (exit $status); see $log"
    printf '{"failed": true, "exit_status": %d, "requested_triangles": %d, "log": "orbit-%s.log"}\n' \
      "$status" "$triangles" "$name" >"$json"
  fi
  return 0
}

run_size 1m 1000000
run_size 5m 5000000
run_size 10m 10000000
run_size 30m 30000000

python3 - "$out" "$label" <<'PY'
import json, sys, pathlib
out, label = pathlib.Path(sys.argv[1]), sys.argv[2]
rows, device = [], None
for name in ["1m", "5m", "10m", "30m"]:
    p = out / f"orbit-{name}.json"
    if not p.exists():
        continue
    r = json.loads(p.read_text())
    note = " (information only)" if name == "30m" else ""
    if r.get("skipped"):
        rows.append(f"| {name}{note} | {r['requested_triangles']:,} (not run) | skipped: {r['reason']} | | | | |")
        device = device or r.get("device")
        continue
    if r.get("failed"):
        rows.append(f"| {name}{note} | {r['requested_triangles']:,} | failed (exit {r['exit_status']}), see {r['log']} | | | | |")
        continue
    device = r["device"]
    f, mem = r["frame_ms"], r["memory"]
    rss = mem.get("process_rss_bytes")
    rows.append(
        f"| {name}{note} | {r['requested_triangles']:,} ({r['triangles']:,}) | {r['fps']:.0f} | "
        f"{f['p50_ms']:.2f} / {f['p95_ms']:.2f} / {f['p99_ms']:.2f} / {f['max_ms']:.1f} | "
        f"{r['cpu_ms']['p95_ms']:.2f} | {r['gpu_time']} | "
        f"{'%.0f MB' % (rss / 2**20) if rss is not None else 'unavailable: ' + str(mem.get('rss_error'))} |"
    )
lines = [f"# Orbit benchmark: {label}", ""]
if device:
    d = device
    lines += [
        f"- CPU: {d['cpu']['model']} ({d['cpu']['physical_cores']} physical / {d['cpu']['logical_cores']} logical cores)",
        f"- GPU: {d['gpu']['name']} ({d['gpu']['backend']}, {d['gpu']['device_type']}, driver '{d['gpu']['driver']}' {d['gpu']['driver_info']})",
        f"- RAM: {d['memory_bytes'] / 2**30:.0f} GB; OS: {d['os']}",
        "",
    ]
lines += [
    "Requested (actual) triangles; frame = encode + submit through GPU completion; fps = 1 / p50 frame.",
    "",
    "| size | triangles | fps | frame p50 / p95 / p99 / worst (ms) | CPU p95 (ms) | GPU timestamps | RSS |",
    "|---|---|---|---|---|---|---|",
    *rows,
    "",
]
(out / "orbit-summary.md").write_text("\n".join(lines))
print("\n".join(lines))
PY
