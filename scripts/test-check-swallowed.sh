#!/usr/bin/env bash
# Tests for check-swallowed-defaults.sh (MF-ISSUE-022).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
dir="$(mktemp -d /tmp/mf-swallow-test.XXXXXX)"
fail=0
expect() { # expected-exit description
  local want="$1"; shift
  set +e; "$here/check-swallowed-defaults.sh" "$dir" > "$dir.out" 2>&1; local got=$?; set -e
  if [ "$got" != "$want" ]; then echo "FAIL: $* (exit $got, wanted $want)"; cat "$dir.out"; fail=1; else echo "ok: $*"; fi
}
mkdir -p "$dir/src"
for pat in 'x.unwrap_or_default()' 'x.unwrap_or(0)' 'x.parse().ok()' 'let _ = f();'; do
  printf 'fn a() {\n    %s\n}\n' "$pat" > "$dir/src/a.rs"
  expect 1 "unmarked: $pat"
  printf 'fn a() {\n    %s // allow-swallow: reason\n}\n' "$pat" > "$dir/src/a.rs"
  expect 0 "same-line marker: $pat"
  printf 'fn a() {\n    // allow-swallow: reason\n    %s\n}\n' "$pat" > "$dir/src/a.rs"
  expect 0 "marker on the line above: $pat"
  printf 'fn a() {\n    // allow-swallow:\n    %s\n}\n' "$pat" > "$dir/src/a.rs"
  expect 1 "marker without a reason: $pat"
done
printf 'fn a() {\n    x.unwrap_or_default(); // allow-swallow: ok\n    y.unwrap_or_default();\n}\n' > "$dir/src/a.rs"
expect 1 "a marked line doesn't cover a second identical line"
rm -rf "$dir" "$dir.out"
exit $fail
