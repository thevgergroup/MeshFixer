#!/usr/bin/env bash
# MF-ISSUE-004: `unwrap_or_default()` in production code can hide an error
# behind an empty value. Each remaining use must be listed, with its reason,
# in scripts/allow-unwrap-or-default.txt as "path: trimmed source line".
set -euo pipefail
allow="scripts/allow-unwrap-or-default.txt"
status=0
while IFS= read -r hit; do
  file="${hit%%:*}"
  rest="${hit#*:}"; code="${rest#*:}"
  code="$(echo "$code" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')"
  if ! grep -qxF "$file: $code" <(grep -v '^#' "$allow" | sed 's/  *# .*$//'); then
    echo "unlisted unwrap_or_default(): $file: $code"
    status=1
  fi
done < <(grep -rn "unwrap_or_default()" crates/*/src | grep -v "/tests/" || true)
exit $status
