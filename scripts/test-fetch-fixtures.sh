#!/usr/bin/env bash
# Tests for fetch-fixtures.sh: plain-file (`-` member) and zip entries,
# both hashes checked, mismatches are hard errors that leave nothing behind.
# Uses file:// URLs, so it needs no network.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
dir="$(mktemp -d /tmp/mf-fetch-test.XXXXXX)"
fail=0
sha256() {
  if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi
}
expect() { # expected-exit description
  local want="$1"; shift
  set +e
  MF_FIXTURE_MANIFEST="$dir/manifest.txt" MF_FIXTURE_DEST="$dir/real" \
    "$here/fetch-fixtures.sh" > "$dir/out" 2>&1
  local got=$?
  set -e
  if [ "$got" != "$want" ]; then echo "FAIL: $* (exit $got, wanted $want)"; cat "$dir/out"; fail=1; else echo "ok: $*"; fi
}
mkdir -p "$dir/src"
printf 'solid a\nendsolid a\n' > "$dir/src/plain.stl"
plain="$(sha256 "$dir/src/plain.stl")"
printf 'o b\n' > "$dir/src/member.obj"
member="$(sha256 "$dir/src/member.obj")"
(cd "$dir/src" && zip -q pack.zip member.obj)
pack="$(sha256 "$dir/src/pack.zip")"
bad=0000000000000000000000000000000000000000000000000000000000000000

echo "plain.stl  $plain  -  $plain  file://$dir/src/plain.stl" > "$dir/manifest.txt"
expect 0 "plain download with matching hash is fetched"
[ "$(sha256 "$dir/real/plain.stl")" = "$plain" ] || { echo "FAIL: plain file content"; fail=1; }
expect 0 "already-fetched plain file is re-hashed, not re-fetched"

rm -rf "$dir/real"
echo "plain.stl  $bad  -  $bad  file://$dir/src/plain.stl" > "$dir/manifest.txt"
expect 1 "plain download with the wrong hash is an error"
[ ! -e "$dir/real/plain.stl" ] || { echo "FAIL: unverified plain file left behind"; fail=1; }

echo "plain.stl  $plain  -  $bad  file://$dir/src/plain.stl" > "$dir/manifest.txt"
expect 1 "plain download whose two hashes differ is an error"
[ ! -e "$dir/real/plain.stl" ] || { echo "FAIL: unverified plain file left behind"; fail=1; }

echo "member.obj  $pack  member.obj  $member  file://$dir/src/pack.zip" > "$dir/manifest.txt"
expect 0 "zip member with matching hashes is fetched"
[ "$(sha256 "$dir/real/member.obj")" = "$member" ] || { echo "FAIL: member content"; fail=1; }

rm -rf "$dir/real"
echo "member.obj  $pack  member.obj  $bad  file://$dir/src/pack.zip" > "$dir/manifest.txt"
expect 1 "zip member with the wrong hash is an error"
[ ! -e "$dir/real/member.obj" ] || { echo "FAIL: unverified member left behind"; fail=1; }

rm -rf "$dir"
exit $fail
