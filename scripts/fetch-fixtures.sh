#!/usr/bin/env bash
# Fetch the real scan fixtures listed in fixtures/manifest.txt into
# fixtures/real/, verifying the SHA-256 of each downloaded archive and of the
# extracted file. Any mismatch is a hard error; nothing unverified is left in
# fixtures/real/. Already-fetched files are re-hashed, not re-downloaded.
#
# An archive_member of `-` means the download is the file itself (no zip):
# its archive and member hashes must then be the same, and both are checked.
#
# Downloads give up rather than hang: 15 s to connect, one retry, and a
# transfer that stays under 10 kB/s for 60 s is abandoned (large files may
# take as long as they need while they keep moving).
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
manifest="${MF_FIXTURE_MANIFEST:-$root/fixtures/manifest.txt}"
dest="${MF_FIXTURE_DEST:-$root/fixtures/real}"
mkdir -p "$dest"

sha256() {
  if command -v sha256sum >/dev/null; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

while read -r name archive_sha member member_sha url; do
  case "$name" in '' | '#'*) continue ;; esac
  out="$dest/$name"
  if [ -f "$out" ]; then
    have="$(sha256 "$out")"
    if [ "$have" = "$member_sha" ]; then
      echo "ok       $name"
      continue
    fi
    echo "error: $out has SHA-256 $have, expected $member_sha" >&2
    echo "       delete it and re-run to fetch a fresh copy" >&2
    exit 1
  fi
  if [ "$member" = "-" ] && [ "$archive_sha" != "$member_sha" ]; then
    echo "error: $name is a plain download (member -) but its two hashes differ" >&2
    exit 1
  fi
  echo "fetching $name from $url"
  curl -sSfL --connect-timeout 15 --retry 1 --speed-limit 10000 --speed-time 60 \
    -o "$work/archive.zip" "$url"
  have="$(sha256 "$work/archive.zip")"
  if [ "$have" != "$archive_sha" ]; then
    echo "error: archive for $name has SHA-256 $have, expected $archive_sha" >&2
    exit 1
  fi
  if [ "$member" = "-" ]; then
    mv "$work/archive.zip" "$out"
    echo "ok       $name"
    continue
  fi
  unzip -q -o "$work/archive.zip" "$member" -d "$work/x"
  have="$(sha256 "$work/x/$member")"
  if [ "$have" != "$member_sha" ]; then
    echo "error: $member has SHA-256 $have, expected $member_sha" >&2
    exit 1
  fi
  mv "$work/x/$member" "$out"
  rm -rf "$work/x" "$work/archive.zip"
  echo "ok       $name"
done < "$manifest"
