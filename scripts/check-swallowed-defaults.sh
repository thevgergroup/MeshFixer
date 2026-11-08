#!/usr/bin/env bash
# MF-ISSUE-004/022: patterns that can hide an error behind a default value
# (`unwrap_or_default()`, `unwrap_or(0)`, `.ok()`, `let _ =`) are allowed in
# production code only with a reviewed reason: an `// allow-swallow: <reason>`
# comment on the same line or on the line directly above.
# Usage: check-swallowed-defaults.sh [dir...]   (default: crates/*/src)
set -euo pipefail
cd "$(dirname "$0")/.."
dirs=("$@")
[ ${#dirs[@]} -eq 0 ] && dirs=(crates/*/src)
status=0
while IFS= read -r file; do
  awk -v file="$file" '
    function marked(s) { return s ~ /\/\/ allow-swallow: *[^ ]/ }
    {
      line = $0
      code = line; sub(/\/\/.*$/, "", code)
      if (code ~ /unwrap_or_default\(\)|unwrap_or\(0\)|\.ok\(\)|let _ =/) {
        if (!marked(line) && !(prev ~ /^[ \t]*\/\// && marked(prev))) {
          printf "%s:%d: unreviewed swallow: %s\n", file, NR, line
          bad = 1
        }
      }
      prev = line
    }
    END { exit bad }
  ' "$file" || status=1
done < <(find "${dirs[@]}" -name '*.rs' -not -path '*/tests/*' | sort)
exit $status
