#!/usr/bin/env bash
# M4 acceptance demo: build the release app and CLI, stage the demo inputs,
# and open MeshFixer at a 1440x900 window with ordinary mouse/trackpad
# controls. Walk through docs/milestones/m4-demo.md.
#
# Inputs staged in artifacts/m4/demo/ (git-ignored; see task-16-brief.md):
#   - demo-5m-damaged.obj  : a 5M-triangle damaged sphere, generated fresh
#                            each run with `meshfixer gen-fixture` (large;
#                            never committed).
#   - demo-scene.3mf       : the two-object transformed scene (a damaged
#                            scan + a plate with a hole/crack/tangled edge),
#                            generated fresh each run.
#   - sphere-three-holes.obj : kept in the repo (small, hand-picked holes of
#                            55/94/181 edges) for the Smooth vs Curvature
#                            step; not regenerated.
# The real Archaeopteryx scan is fetched separately by
# scripts/fetch-fixtures.sh into fixtures/real/ (git-ignored) and used from
# there directly — it is never copied into artifacts/.
#
# Usage: scripts/demo.sh [--dry-run]
#   --dry-run : stage the inputs and print the launch command, but don't
#               build the release binaries or open the app. Used to verify
#               this script without putting a window on screen.
set -euo pipefail
cd "$(dirname "$0")/.."
root="$(pwd)"

dry_run=0
case "${1:-}" in
  --dry-run) dry_run=1 ;;
  "") ;;
  *) echo "usage: $0 [--dry-run]" >&2; exit 2 ;;
esac

toolchain="$HOME/.rustup/toolchains/$(sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml)-$(rustc -vV 2>/dev/null | sed -n 's/^host: //p')/bin"
[ -d "$toolchain" ] && export PATH="$toolchain:$PATH"
export CARGO_INCREMENTAL=0

demo="$root/artifacts/m4/demo"
mkdir -p "$demo"

sphere_three_holes="$demo/sphere-three-holes.obj"
if [ ! -f "$sphere_three_holes" ]; then
  echo "error: $sphere_three_holes is missing (it ships in the repo; see artifacts/m4/demo/)" >&2
  exit 1
fi

real_scan="$root/fixtures/real/archaeopteryx.obj"
five_m="$demo/demo-5m-damaged.obj"
scene="$demo/demo-scene.3mf"
cli="$root/target/release/meshfixer"
app="$root/target/release/meshfixer-desktop"

if [ "$dry_run" -eq 1 ]; then
  echo "dry run: would build target/release/{meshfixer,meshfixer-desktop} (-j 2)"
  echo "dry run: would fetch the real scan with scripts/fetch-fixtures.sh -> $real_scan"
  echo "dry run: would generate $five_m with: $cli gen-fixture damaged $five_m --triangles 5000000"
  echo "dry run: would generate $scene with: $cli gen-fixture scene $scene"
  echo "staged already: $sphere_three_holes"
  echo "launch command: $app"
  exit 0
fi

echo "== building release app and CLI (-j 2) =="
cargo build -j 2 --release -p mf-desktop -p mf-cli

echo "== fetching the real Archaeopteryx scan (fixtures/real/, git-ignored) =="
"$root/scripts/fetch-fixtures.sh"

echo "== generating the 5M-triangle damaged sphere (not committed) =="
"$cli" gen-fixture damaged "$five_m" --triangles 5000000

echo "== generating the two-object transformed demo scene (not committed) =="
"$cli" gen-fixture scene "$scene"

cat <<EOF

Demo inputs staged in $demo:
  - $five_m (generated)
  - $real_scan (fetched; used in place, not copied)
  - $scene (generated)
  - $sphere_three_holes (kept in the repo)

Launch command (also run below): $app
EOF

exec "$app"
