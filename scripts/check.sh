#!/usr/bin/env bash
# Run the same checks as CI's push job before pushing.
#
# Fast by default: tests run via `scripts/run-tests.sh --fast`, which skips
# the timing/heavy (5M-triangle) test groups. Those run ON DEMAND only, once
# work is complete and proven (owner ruling, 2026-11-09): GitHub Actions →
# "Heavy tests" → Run workflow (heavy.yml), e.g. before merging a milestone,
# or locally with `scripts/check.sh --full`. Never after every change.
set -euo pipefail
cd "$(dirname "$0")/.."

mode="--fast"
case "${1:-}" in
  --full) mode="" ;;
  "") ;;
  *) echo "usage: $0 [--full]" >&2; exit 2 ;;
esac

toolchain="$HOME/.rustup/toolchains/$(sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml)-$(rustc -vV | sed -n 's/^host: //p')/bin"
[ -d "$toolchain" ] && export PATH="$toolchain:$PATH"
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
scripts/test-check-swallowed.sh > /dev/null
scripts/test-fetch-fixtures.sh > /dev/null
scripts/check-swallowed-defaults.sh
cargo deny check licenses 2>/dev/null
# Timing tests run on their own after the rest (scripts/run-tests.sh), unless
# running in fast mode (the default here), which skips them entirely.
# A unique temp file per run (not a fixed path) so concurrent check.sh runs
# (e.g. another agent/session testing at the same time) can't clobber or
# interleave each other's output.
test_log="$(mktemp "${TMPDIR:-/tmp}/meshfixer-check-tests.XXXXXX")"
trap 'rm -f "$test_log"' EXIT
tests=0
scripts/run-tests.sh $mode > "$test_log" 2>&1 || tests=$?
grep -E "^test result: FAILED|panicked|worst frame|fast mode:" "$test_log" || true
scripts/ci-test-summary.sh "$test_log" "${MF_SKIP_LOG:-/nonexistent}"
! grep -q "^test result: FAILED" "$test_log"
[ "$tests" -eq 0 ]
