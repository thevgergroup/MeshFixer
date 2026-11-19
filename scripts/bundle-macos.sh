#!/usr/bin/env bash
# Builds the release desktop app and the CLI, then assembles
# dist/MeshFixer.app (Info.plist, the icon, the binary) and
# dist/MeshFixer-<version>-macos.zip.
#
# Signing (optional): MF_SIGN_IDENTITY="Developer ID Application: …" signs the
# CLI and the app with the hardened runtime and a secure timestamp.
# Notarising (optional, needs signing): MF_NOTARY_PROFILE=<name> submits the
# zip to Apple with `xcrun notarytool` (credentials stored once with
# `xcrun notarytool store-credentials <name>`), waits, and staples the ticket.
# Without either, the app is unsigned: open it the first time with right-click > Open. Open models with
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
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources/bin"
cp "$target/release/meshfixer-desktop" "$app/Contents/MacOS/MeshFixer"
# The CLI lives apart from the app binary: macOS file names are
# case-insensitive, so "meshfixer" in Contents/MacOS would overwrite
# "MeshFixer".
cp "$target/release/meshfixer" "$app/Contents/Resources/bin/meshfixer"
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
if [ -n "${MF_SIGN_IDENTITY:-}" ]; then
  # Inside out: the nested CLI first, then the app itself.
  codesign --force --options runtime --timestamp --sign "$MF_SIGN_IDENTITY" \
    "$app/Contents/Resources/bin/meshfixer"
  codesign --force --options runtime --timestamp --sign "$MF_SIGN_IDENTITY" "$app"
  codesign --verify --deep --strict --verbose=2 "$app"
fi
(cd "$dist" && ditto -c -k --keepParent MeshFixer.app "$zip")
if [ -n "${MF_NOTARY_PROFILE:-}" ]; then
  [ -n "${MF_SIGN_IDENTITY:-}" ] || { echo "error: notarising needs MF_SIGN_IDENTITY" >&2; exit 1; }
  xcrun notarytool submit "$zip" --keychain-profile "$MF_NOTARY_PROFILE" --wait
  xcrun stapler staple "$app"
  xcrun stapler validate "$app"
  spctl --assess --type execute --verbose=2 "$app"
  # Re-zip so the download carries the stapled ticket.
  (cd "$dist" && ditto -c -k --keepParent MeshFixer.app "$zip.stapled" && mv "$zip.stapled" "$zip")
fi
echo "$app"
echo "$zip"
