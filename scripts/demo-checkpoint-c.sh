#!/usr/bin/env bash
# Checkpoint C demo: open the real desktop app directly on the real
# Archaeopteryx scan (fixtures/real/archaeopteryx.obj), so the owner can
# try the surgical workflow themselves (units prompt, inspect, select and
# fill a hole, batch fill, mark an intentional opening, export). Uses the
# release build already at target/release/meshfixer-desktop; rebuilds only
# if it's missing.
set -euo pipefail
cd "$(dirname "$0")/.."
root="$(pwd)"
toolchain="$HOME/.rustup/toolchains/$(sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml)-$(rustc -vV | sed -n 's/^host: //p')/bin"
[ -d "$toolchain" ] && export PATH="$toolchain:$PATH"
export CARGO_INCREMENTAL=0

scan="$root/fixtures/real/archaeopteryx.obj"
if [ ! -f "$scan" ]; then
  echo "fixtures/real/archaeopteryx.obj is missing; fetch it first:" >&2
  echo "  scripts/fetch-fixtures.sh" >&2
  exit 1
fi

app="$root/target/release/meshfixer-desktop"
if [ ! -x "$app" ]; then
  cargo build --release -j 4 -p mf-desktop
fi
exec "$app" "$scan"
