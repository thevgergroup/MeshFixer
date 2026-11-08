#!/usr/bin/env bash
# Summarize a `cargo test` log for the CI job summary, separating verified
# passes from GPU tests that were skipped (they return early and would
# otherwise be counted as passed).
set -euo pipefail
log="$1"
skips="${2:-}"
if [ ! -s "$log" ]; then
  echo "### Tests on ${RUNNER_OS:-local}: **did not run** (an earlier step failed)" >> "${GITHUB_STEP_SUMMARY:-/dev/stdout}"
  exit 0
fi
passed=$(grep -E '^test result:' "$log" | awk '{s+=$4} END {print s+0}')
failed=$(grep -E '^test result:' "$log" | awk '{s+=$6} END {print s+0}')
skipped=0
if [ -n "$skips" ] && [ -f "$skips" ]; then skipped=$(grep -c . "$skips" || true); fi
verified=$((passed - skipped))
{
  echo "### Tests on ${RUNNER_OS:-local}"
  echo ""
  echo "| Verified passes | Skipped (no GPU) | Failed |"
  echo "|---|---|---|"
  echo "| $verified | $skipped | $failed |"
  if [ "$skipped" -gt 0 ]; then
    echo ""
    echo "Skipped GPU tests (NOT verified on this runner):"
    sed 's/^/- /' "$skips"
  fi
} >> "${GITHUB_STEP_SUMMARY:-/dev/stdout}"
