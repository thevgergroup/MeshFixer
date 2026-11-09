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
# widening them. The heavy 5M memory tests run there too. Both steps
# always run; the exit status is non-zero if either failed.
set -uo pipefail
cd "$(dirname "$0")/.."

# package, test target, test-name filters (substrings; each is also
# skipped in step 1).
timing=(
  "mf-render bvh face_pick_under_16ms_at_5m bvh_build_is_cancellable brute_force_pick_is_cancellable"
  "mf-desktop checkpoint_b checkpoint_b_5m_mesh checkpoint_b_real_scan"
  "mf-desktop checkpoint_b_followup checkpoint_b_followup_real_scan"
  "mf-geometry topology_pool a_second_caller_never_waits_for_another_threads_computation topology_requests_on_one_pool_all_finish"
  "mf-geometry topology_cancel topology_checkpoints_every_100ms_around_a_million_face_hub"
  "mf-command cancel checkpoints_at_least_every_100ms cancel_to_stop_within_250ms_on_5m_import_and_fill"
  "mf-desktop progress ui_frames_stay_under_50ms cancel_is_prompt cancel_button_stops_fill escape_cancels_the_job_first ui_frames_stay_under_50ms_while_hovering"
  "mf-desktop layout clicking_a_target_shows_its_operations_in_properties"
  # 5M memory measurements (5M requested, ≈4.8M actual): alone, so no other test's 5M mesh is in memory
  # at the same time (and the allocator counts only their own process).
  "mf-geometry topology_memory topology_build_peak_under_150mb_at_5m"
  "mf-cli bench_memory working_set_under_400mb_at_5m"
  "mf-desktop memory realistic_session_rss_is_reported"
  # The messy-model collection (15 real files, one of 5.7M triangles):
  # alone, so its recorded times and peak heap are its own.
  "mf-operations messy_corpus every_messy_model_imports_and_analyses every_messy_model_fills_or_refuses_by_name targeted_fill_on_messy_models_is_untouched_outside messy_results_are_recorded"
  # A 1M-triangle orbit on the GPU (a smoke test, not an fps gate): alone,
  # so its frames don't compete with other GPU tests.
  "mf-cli bench_orbit bench_orbit_json_has_device_info_and_percentiles"
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
