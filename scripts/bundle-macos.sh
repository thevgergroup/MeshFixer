#!/usr/bin/env bash
# Builds the release desktop app and the CLI, then assembles
# dist/MeshFixer.app (Info.plist, the icon, the binary) and
# dist/MeshFixer-<version>-macos.zip. Not signed or notarised: on another Mac,
# open it the first time with right-click > Open. Open models with
# File > Open or by dropping them on the window (Finder "Open With" isn't
# wired up yet).
#
# Usage: scripts/bundle-macos.sh
set -euo pipefail
cd "$(dirname "$0")/.."
root="$(pwd)"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[ -n "$version" ] || { echo "error: no version in Cargo.toml" >&2; exit 1; }

cargo build --release -j "${CARGO_BUILD_JOBS:-4}" -p mf-desktop -p mf-cli
target="${CARGO_TARGET_DIR:-$root/target}"

dist="$root/dist"
app="$dist/MeshFixer.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$target/release/meshfixer-desktop" "$app/Contents/MacOS/MeshFixer"
cp "$target/release/meshfixer" "$app/Contents/MacOS/meshfixer"
cp crates/mf-desktop/assets/icon/MeshFixer.icns "$app/Contents/Resources/MeshFixer.icns"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>MeshFixer</string>
  <key>CFBundleDisplayName</key><string>MeshFixer</string>
  <key>CFBundleIdentifier</key><string>com.thevgergroup.meshfixer</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleExecutable</key><string>MeshFixer</string>
  <key>CFBundleIconFile</key><string>MeshFixer</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

zip="$dist/MeshFixer-$version-macos.zip"
(cd "$dist" && ditto -c -k --keepParent MeshFixer.app "$zip")
echo "$app"
echo "$zip"
