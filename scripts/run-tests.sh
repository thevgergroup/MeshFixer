#!/usr/bin/env bash
# Run the workspace tests the way CI does, in two steps:
#
#  1. everything, in parallel, except the wall-clock-sensitive tests;
#  2. those timing tests alone, one at a time (--test-threads=1), with their
#     output shown (--nocapture), so a gated frame's slowest phase is in the
#     log.
#
# A full parallel run starves the timing tests (cancel within 250 ms,
# checkpoints every 100 ms, UI frames under 50 ms, a UI that settles in a
# few steps); isolating them keeps their thresholds meaningful without
# widening them. Both steps always run; the exit status is non-zero if
# either failed.
set -uo pipefail
cd "$(dirname "$0")/.."

# package, test target, test-name filters (substrings; each is also
# skipped in step 1).
timing=(
  "mf-render bvh face_pick_under_16ms_at_5m bvh_build_is_cancellable brute_force_pick_is_cancellable"
  "mf-desktop checkpoint_b checkpoint_b_5m_mesh checkpoint_b_real_scan"
  "mf-desktop checkpoint_b_followup checkpoint_b_followup_real_scan"
  "mf-geometry topology_pool a_second_caller_never_waits_for_another_threads_computation topology_requests_on_one_pool_all_finish"
  "mf-command cancel checkpoints_at_least_every_100ms cancel_to_stop_within_250ms_on_5m_import_and_fill"
  "mf-desktop progress ui_frames_stay_under_50ms cancel_is_prompt cancel_button_stops_fill escape_cancels_the_job_first ui_frames_stay_under_50ms_while_hovering"
  "mf-desktop layout clicking_a_target_shows_its_operations_in_properties"
)

skips=()
for entry in "${timing[@]}"; do
  read -r -a words <<<"$entry"
  for name in "${words[@]:2}"; do
    skips+=(--skip "$name")
  done
done

status=0
echo "=== step 1: workspace tests in parallel (timing tests excluded) ==="
cargo test --workspace -- "${skips[@]}" || status=1

echo "=== step 2: timing tests, one at a time ==="
for entry in "${timing[@]}"; do
  read -r -a words <<<"$entry"
  cargo test -p "${words[0]}" --test "${words[1]}" -- --test-threads=1 --nocapture "${words[@]:2}" || status=1
done
exit $status
