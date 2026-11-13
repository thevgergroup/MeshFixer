#!/usr/bin/env bash
# Summarize a `cargo test` log for the CI job summary, separating verified
# passes from GPU tests that were skipped (they return early and would
# otherwise be counted as passed), and from heavy/timing test groups that
# scripts/run-tests.sh --fast didn't run at all (they never ran, so they're
# not in the pass count either — this just surfaces them as their own
# category instead of silently missing from the total).
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
fast_mode_line=$(grep -E '^fast mode:' "$log" || true)
fast_skipped=0
if [ -n "$fast_mode_line" ]; then
  fast_skipped=$(echo "$fast_mode_line" | grep -oE '^fast mode: [0-9]+' | awk '{print $3}')
fi
{
  echo "### Tests on ${RUNNER_OS:-local}"
  echo ""
  echo "| Verified passes | Skipped (no GPU or fixture) | Skipped by mode (fast) | Failed |"
  echo "|---|---|---|---|"
  echo "| $verified | $skipped | $fast_skipped | $failed |"
  if [ "$skipped" -gt 0 ]; then
    echo ""
    echo "Skipped GPU tests (NOT verified on this runner):"
    sed 's/^/- /' "$skips"
  fi
  if [ "$fast_skipped" -gt 0 ]; then
    echo ""
    echo "Skipped by mode, NOT run and NOT counted as passed ($fast_mode_line):"
  fi
} >> "${GITHUB_STEP_SUMMARY:-/dev/stdout}"
