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
#
# --fast: run step 1 only (still excluding the timing list below) and skip
# step 2 entirely. For local iteration; CI always runs both steps via a
# plain (no-flag) invocation, so the timing/5M tests are verified on every
# push regardless of this flag.
set -uo pipefail
cd "$(dirname "$0")/.."

fast=0
case "${1:-}" in
  --fast) fast=1 ;;
  "") ;;
  *) echo "usage: $0 [--fast]" >&2; exit 2 ;;
esac

# package, test target, test-name filters (substrings; each is also
# skipped in step 1).
timing=(
  "mf-render bvh face_pick_under_16ms_at_5m bvh_build_is_cancellable brute_force_pick_is_cancellable"
  # A worker's GPU wait must not stall other threads' GPU calls (MF-RT-001).
  "mf-render gpu_wait a_worker_waiting_for_the_gpu_does_not_stall_other_threads_gpu_calls"
  "mf-desktop checkpoint_b checkpoint_b_5m_mesh checkpoint_b_real_scan"
  "mf-desktop checkpoint_b_followup checkpoint_b_followup_real_scan"
  "mf-geometry topology_pool a_second_caller_never_waits_for_another_threads_computation topology_requests_on_one_pool_all_finish"
  "mf-geometry topology_cancel topology_checkpoints_every_100ms_around_a_million_face_hub"
  "mf-command cancel checkpoints_at_least_every_100ms cancel_to_stop_within_250ms_on_5m_import_and_fill"
  "mf-desktop progress ui_frames_stay_under_50ms cancel_is_prompt cancel_button_stops_fill escape_cancels_the_job_first ui_frames_stay_under_50ms_while_hovering"
  "mf-desktop layout clicking_a_target_shows_its_operations_in_properties"
  # The Transform tool on 30M triangles (Phase 1 Task 5): a gizmo drag
  # costs no more than an orbit (+1 ms), and Drop to plate keeps UI frames
  # under 50 ms. A 30M sphere in memory: alone.
  "mf-desktop transform dragging_a_30m_object_keeps_frames_under_orbit_plus_1ms ui_frames_stay_under_50ms_while_dropping_30m_to_plate analysis_stays_available_after_moving_a_30m_object turning_a_30m_object_keeps_the_analysis_and_frames_under_50ms"
  # GUI timing under parallel load: the framing animation after
  # select_defect sometimes outlasts Harness::run's step budget when the
  # whole browser file runs in parallel (passes alone; seen in Task 12a).
  "mf-desktop browser intentional_openings_survive_edits_and_are_excluded_from_fill_all"
  # Fairing the largest holes (MF-ISSUE-053): seconds of single-threaded
  # solve, timed, with a 100 ms checkpoint gate.
  "mf-operations fill_holes large_holes_fair_quickly"
  # 5M memory measurements (5M requested, ≈4.8M actual): alone, so no other test's 5M mesh is in memory
  # at the same time (and the allocator counts only their own process).
  "mf-geometry topology_memory topology_build_peak_under_150mb_at_5m"
  "mf-cli bench_memory working_set_under_400mb_at_5m"
  "mf-desktop memory realistic_session_rss_is_reported"
  # The messy-model collection (15 real files, one of 5.7M triangles):
  # alone, so its recorded times and peak heap are its own.
  "mf-operations messy_corpus every_messy_model_imports_and_analyses every_messy_model_fills_or_refuses_by_name targeted_fill_on_messy_models_is_untouched_outside messy_results_are_recorded"
  # mesh.boolean (Phase 1 Task 8): the messy corpus, and the D5 window's
  # timing gates (30M scan ∪ 50k part ≤ 3 s, 1M ∪ 1M ≤ 10 s, cancel inside
  # the kernel ≤ 250 ms). The 30M sphere in memory: alone.
  "mf-operations boolean messy_corpus_booleans_are_valid_or_named union_30m_scan_with_50k_part_under_3s union_1m_by_1m_under_10s cancel_within_250ms_in_kernel"
  # mesh.make_solid on real self-intersecting scans (winged victory, 5.7M).
  "mf-operations make_solid make_solid_on_real_self_intersecting_scans"
  # A 1M-triangle orbit on the GPU (a smoke test, not an fps gate): alone,
  # so its frames don't compete with other GPU tests.
  "mf-cli bench_orbit bench_orbit_json_has_device_info_and_percentiles"
)

# Skipped here and run by nobody in this script: the boolean kernel
# qualification (Phase 1 Tasks 7 and 7b, ADR-0002) is run by hand (heavy.yml
# `kernel-qualification`, manual dispatch only; or locally with
# `cargo test -p mf-boolean --features manifold-rust --test qualify`), so a
# kernel's verdict doesn't turn a milestone's heavy run red.
not_here=(qualify_corpus qualify_windows qualify_1m_union)

skips=()
for name in "${not_here[@]}"; do
  skips+=(--skip "$name")
done
for entry in "${timing[@]}"; do
  read -r -a words <<<"$entry"
  for name in "${words[@]:2}"; do
    skips+=(--skip "$name")
  done
done

status=0
echo "=== step 1: workspace tests in parallel (timing tests and GUI tests excluded) ==="
# --no-fail-fast: a failing test target must not stop the rest of the
# workspace from running, or a single failure hides every other result.
cargo test --workspace --exclude mf-desktop --no-fail-fast -- "${skips[@]}" || status=1

echo "=== step 1b: desktop GUI tests, one at a time ==="
# The egui_kittest GUI tests drive real frames on a shared GPU device and wait
# on wall-clock conditions; run in parallel (especially on a loaded machine)
# they time out at random. One at a time they are deterministic.
cargo test -p mf-desktop --no-fail-fast -- --test-threads=1 "${skips[@]}" || status=1

if [ "$fast" -eq 1 ]; then
  echo "fast mode: ${#timing[@]} heavy/timing test groups skipped — run them on demand: Actions → "Heavy tests" (heavy.yml) or scripts/check.sh --full"
  exit $status
fi

echo "=== step 2: timing tests, one at a time ==="
for entry in "${timing[@]}"; do
  read -r -a words <<<"$entry"
  # The desktop's timing tests (MF-RT-001's frame gates among them) time
  # the app itself, so they measure it as shipped: optimised (--release).
  # Unoptimised, mf-desktop's own UI code (the tree's rows) took ~25 ms of
  # a 5M frame, which the gate would have blamed on the product (M4 final
  # review I4).
  profile=""
  [ "${words[0]}" = mf-desktop ] && profile="--release"
  # shellcheck disable=SC2086 # $profile is empty or one flag
  cargo test $profile -p "${words[0]}" --test "${words[1]}" --no-fail-fast -- --test-threads=1 --nocapture "${words[@]:2}" || status=1
done
exit $status
