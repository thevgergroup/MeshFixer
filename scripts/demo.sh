#!/usr/bin/env bash
# Build the release desktop app and open it on the Checkpoint A demo scene:
# two objects at their own 3MF transforms (a damaged scan with holes and
# loose pieces, and a plate with a hole, a narrow crack and a tangled edge),
# generated into artifacts/m4/demo/. The window opens at 1440×900.
set -euo pipefail
cd "$(dirname "$0")/.."
root="$(pwd)"
toolchain="$HOME/.rustup/toolchains/$(sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml)-$(rustc -vV | sed -n 's/^host: //p')/bin"
[ -d "$toolchain" ] && export PATH="$toolchain:$PATH"
export CARGO_INCREMENTAL=0

cargo build --release -p mf-desktop -p mf-cli
demo="$root/artifacts/m4/demo"
mkdir -p "$demo"
"$root/target/release/meshfixer" gen-fixture scene "$demo/demo-scene.3mf"
exec "$root/target/release/meshfixer-desktop" "$demo/demo-scene.3mf"
