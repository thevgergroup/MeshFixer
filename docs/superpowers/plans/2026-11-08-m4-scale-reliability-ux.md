# M4 Scale, Reliability and Interaction Quality Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** MeshFixer stays responsive on 5–10M-triangle damaged meshes. Long work runs in a bounded pool with live progress and cancellation that leaves the document untouched. The viewport updates incrementally and draws every object at its transform. Finding, selecting and repairing a defect feels natural. The milestone ends with a UI/UX demo for the project owner.

**Architecture:**
- **`mf-runtime`:** a new, dependency-light crate with a sized job pool (rayon), cancellation tokens and monotonic progress. It's threaded through import, topology and every operation as a `RunContext`. Operations stay transactional: a cancelled run returns `Cancelled`, and nothing commits.
- **Renderer:**
  - Per-object model matrices.
  - GPU geometry in per-face-chunk buffers aligned with the 32,768-face storage chunks, so a revision re-uploads only the chunks whose faces or referenced vertices changed.
  - A packed per-face state buffer that carries selection and overlay tints, so a selection change never touches geometry.
- **Picking:** a BVH built off the UI thread.
- **Desktop:** gains a defect browser, hover feedback, camera framing, a unit prompt for unitless files, a progress/cancel bar and a metrics panel.

**Tech Stack:** As M3 (Rust 1.99.1, eframe/egui 0.37, wgpu 30), plus `cargo-fuzz` (nightly toolchain, fuzz job only) and `sysinfo` (MIT) for benchmark device info.

**Spec:** `docs/spec/phase0-spec.md` v0.3: MF-RT-001…005, MF-CMD-006, MF-RENDER-002 (preliminary, M2 Max), MF-RENDER-003/004/005, MF-GEO-002/003, MF-DOC-002, MF-IO-002, exit criterion 9. Milestone: `docs/plan/phase0-milestones.md` § M4. Issues:
- 042 (mandatory, first)
- 019, 020, 041, 045
- the minors 033–040 and 046
- 032 is roadmap only; it isn't in this plan's scope

## Global Constraints

- **Unchanged from M3:**
  - MIT/Apache licences and Rust 1.99.1 (fuzzing alone uses nightly).
  - Deterministic results at any thread count.
  - No silent fallbacks: reviewed defaults carry `// allow-swallow: <reason>`.
  - `cargo fmt --all && scripts/check.sh && git commit`, never `;`.
  - Never switch branches with uncommitted changes. Every delete uses an absolute path.
- **No geometry on the UI thread:** no operation, import, export, O(n) topology, BVH or vertex-buffer build. No UI frame exceeds **50 ms** while `fill_holes` runs on 5M triangles (MF-RT-001).
- **Cancellation:** long work checks its token at least every **100 ms**; cancel-to-stop is **≤ 250 ms** on a 5M import and a large `fill_holes`. Cancellation leaves the document hash unchanged and produces no revision (MF-RT-002, MF-CMD-006).
- **Progress** is monotonic in 0–1, with a stage label, in the GUI and with CLI `--progress` (MF-RT-003).
- **The pool leaves at least one core free by default.** Its size is configurable (`--threads`, settings) and shown in diagnostics (MF-RT-004).
- **Telemetry:** operation durations, peak heap, GPU buffer bytes, and frame-time p50/p95/p99, in the GUI debug panel and CLI `--metrics` (MF-RT-005).
- **Interactive targets** (MF-RENDER-002) on damaged fixtures: **1M ≥ 60 fps, 5M ≥ 30 fps, 10M ≥ 15 fps**. M2 Max results are *preliminary*, and every result records CPU, GPU, RAM, OS and resolution.
- **Picking:** face picking takes **< 16 ms** at 5M triangles (MF-RENDER-004).
- **Memory:** working geometry plus cached topology at 5M is **≤ 400 MB** (MF-GEO-002). Topology build peak at 5M is **≤ 150 MB** (MF-ISSUE-019).
- **Disk is tight on the dev machine:** 5M/10M fixtures are generated into `mktemp -d` and deleted after use, never written into the repo.
- **Limits are named for what causes them.** An algorithm's limit, such as the 1000-edge DP cap, is reported as that algorithm's limit, never as a product limit.

## Review Focus

1. **Cancel racing completion:** a cancel arriving just as an operation finishes must produce exactly one outcome: either the revision is committed and reported, or nothing changed and "Cancelled" is reported. Never a revision plus a "Cancelled" message. Test: `cancel_racing_completion_has_one_outcome` (Task 3).
2. **History jumps with incremental GPU uploads:** after undo, redo, or checkout to a revision on another branch, the GPU chunks must equal a full rebuild, with no stale chunk from the previous revision. Test: `incremental_upload_matches_full_rebuild_after_checkout` (Task 5).
3. **Mirrored transforms** (negative determinant): faces must still render front-facing and lit correctly, and picking must hit them. Test: `mirrored_object_renders_and_picks` (Task 1).
4. **Thread budget edge cases:** on a 1- or 2-core machine, or with `--threads 0` or more threads than cores, the pool must have at least 1 thread and say what it chose. Test: `pool_size_is_clamped_and_reported` (Task 2).
5. **Hostile archives:** a 3MF whose model part decompresses far beyond its declared or plausible size (a zip bomb), or an STL declaring 4 billion triangles, must fail fast with a structured error and bounded memory. Test: `threemf_decompression_is_bounded` (Task 10).

---

### Task 1: Transform-aware rendering and picking (MF-ISSUE-042, mandatory first)

**Files:**
- `mf-render`: `renderer.rs` (per-draw model matrix uniform), `pick.rs`, `shader.wgsl`, `target.rs`
- `mf-desktop`: `viewport.rs`, `app.rs`
- Test: `crates/mf-render/tests/transforms.rs`, `crates/mf-desktop/tests/multi_object.rs`

**Interfaces (produces):**
- `mf_render::model_matrix(transform: &[f64; 12]) -> DMat4`. 3MF row-major 3×4 order (`m00 m01 m02 m10 … m32`, the last three the translation). The mapping is a row vector times the matrix.
- `pick_loop` and `ray_pick` take `model: &DMat4`. Rays are transformed into object space with the inverse; distances are reported in world space.
- `screen_position(cam, origin, size, world)` is unchanged. Callers pass `model * p`.
- Viewport: draws **every** object in the document at its transform. The active object is full colour; inactive ones are dimmed. The camera fits the union of the transformed bounds.
- When `det < 0`, the pipeline uses front-face `Cw` for that draw, so mirrored objects stay front-facing.

- [ ] **Step 1: Tests**
  - `transformed_objects_render_where_their_transform_puts_them`: a two-object 3MF with translations (−3, 0, 0) and (3, 0, 0). In the offscreen render, both cubes' projected centres are lit, and the gap between them isn't.
  - `pick_hits_a_translated_object_in_world_space`
  - `loop_pick_on_a_rotated_object`
  - `mirrored_object_renders_and_picks` (Review Focus 3)
  - GUI test `two_objects_both_visible_and_clicking_one_activates_it`: clicking an inactive object makes it active and clears a selection on the other.
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(render): draw and pick every object at its transform [MF-ISSUE-042]`, and close 042 in the register. Multi-object editing is then "supported", which the register states.

### Task 2: `mf-runtime`: pool, cancellation, progress (MF-RT-002/003/004)

**Files:** `crates/mf-runtime/{Cargo.toml, src/lib.rs}`; workspace `Cargo.toml`

**Interfaces (produces):**
- **`JobPool::new(threads: Option<usize>) -> (JobPool, PoolChoice)`**
  - `PoolChoice { threads: usize, cores: usize, reason: &'static str }`.
  - Default is `max(1, cores − 1)`. `Some(0)` is clamped to 1. More than `cores` is allowed, but the `reason` says so.
  - `JobPool::install<R>(&self, f: impl FnOnce() -> R) -> R` runs on the pool's rayon `ThreadPool`.
- **`CancelToken`:** `Clone`, `cancel()`, `is_cancelled()`.
- **`RunContext { cancel: CancelToken, progress: Progress }`**
  - `RunContext::checkpoint(&self) -> Result<(), Cancelled>`. It's cheap (an atomic load) and is called at least every 100 ms of work; the rule of thumb is every 32,768 elements.
  - `RunContext::none()` is a context that never cancels, for tests and existing call sites.
- **`Progress`:** `report(stage: &'static str, fraction: f64)`. The fraction is clamped to 0–1 and made monotonic within a stage sequence (it never decreases).
  - `Progress::snapshot() -> (stage, fraction)`, lock-free.
  - `Progress::channel()` returns a sink and a reader for frontends.
- **`#[derive(thiserror::Error)] struct Cancelled`**, with the message "cancelled by the user".

- [ ] **Step 1: Tests**
  - `pool_size_is_clamped_and_reported` (Review Focus 4)
  - `progress_is_monotonic_and_clamped`
  - `checkpoint_fails_after_cancel`
  - `pool_runs_rayon_work_on_its_own_threads` (thread names `mf-pool-N`)
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(runtime): job pool, cancellation and progress [MF-RT-002, MF-RT-003, MF-RT-004]`.

### Task 3: Cancellation and progress through the engine (MF-RT-001/002, MF-CMD-006)

**Files:**
- `mf-operations/src/lib.rs` and every operation
- `mf-geometry/src/topology.rs`, `mesh.rs`
- `mf-io/src/{lib.rs, stl.rs, obj.rs, threemf.rs}`
- `mf-command/src/session.rs`
- Test: `crates/mf-command/tests/cancel.rs`

**Interfaces:**
- **Consumes:** Task 2.
- **Produces:**
  - `Operation::execute(&self, input, target, params, ctx: &RunContext) -> Result<OpOutput, OpError>`.
  - New error variants: `OpError::Cancelled` and `CommandError::Cancelled`.
  - `Mesh::topology_with(&self, ctx: &RunContext) -> Result<Arc<Topology>, Cancelled>`. Nothing is cached on cancel; `topology()` stays as `topology_with(&RunContext::none())`.
  - `mf_io::read_model_with(path, ctx)`. Readers report `"reading"` progress by bytes consumed, and check the token per 32k elements.
  - `Prepared::run(self, ctx: &RunContext) -> Result<Completed, CommandError>`. Once `run` has returned `Ok`, a cancel has no effect: the result commits or is discarded only by `commit`, so there is exactly one outcome.
- **Progress stages:**
  - `fill_holes`: `"topology"`, `"triangulating"` (by loops done), `"refining"`.
  - Import: `"reading"`, `"welding"`.

- [ ] **Step 1: Tests**
  - `cancel_during_fill_holes_leaves_document_unchanged`: on a 5M fixture, cancel at 30%. The document hash is unchanged, there's no new revision, and the error is `Cancelled`.
  - `cancel_to_stop_within_250ms_on_5m_import_and_fill`: release-optimised test crates. Measured from `cancel()` to the return.
  - `cancel_racing_completion_has_one_outcome` (Review Focus 1): 200 iterations, with the cancel scheduled at random offsets around completion.
  - `checkpoints_at_least_every_100ms`: an instrumented context records the largest gap between checkpoints during a 5M `fill_holes` and a 5M import.
  - `progress_reaches_one_on_success`
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(engine): cooperative cancellation and progress in import, topology and operations [MF-RT-002, MF-RT-003, MF-CMD-006]`.

### Task 4: Frontends: progress, cancel, thread budget (MF-RT-003/004)

**Files:** `mf-desktop/src/{app.rs, worker.rs, panels.rs}`, `mf-cli/src/main.rs`

**Interfaces (consumes Tasks 2–3):**
- **Desktop:**
  - One `JobPool` for the app, sized from settings. All workers run on it, replacing the ad-hoc `std::thread::spawn`. Picking and analysis use a separate low-latency lane, so a long operation can't starve a click.
  - A status bar shows the stage, a percentage and a **Cancel** button (shortcut Esc while a job runs, before selection clearing).
  - After a cancel, the status reads "Cancelled: document unchanged".
- **CLI:**
  - `--progress` writes `progress <stage> <0.000–1.000>` lines to stderr, at most 10 per second, ending in 1.000.
  - `--threads N`.
  - SIGINT cancels the running job and exits with code 130 and "cancelled", writing no output file.

- [ ] **Step 1: Tests**
  - GUI `cancel_button_stops_fill_and_document_is_unchanged` (5M fixture)
  - GUI `ui_frames_stay_under_50ms_while_fill_holes_runs_on_5m`: `App::update` duration is instrumented; the maximum must be ≤ 50 ms.
  - CLI `progress_lines_are_monotonic_and_end_at_one`
  - CLI `threads_flag_is_reported_in_metrics`
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(desktop, cli): live progress, cancel and thread budget [MF-RT-001, MF-RT-003, MF-RT-004]`.

### Task 5: Chunked GPU geometry with partial re-upload (MF-RENDER-003, geometry)

**Files:**
- `mf-geometry/src/mesh.rs` (chunk-sharing queries)
- `mf-render/src/gpu_mesh.rs` (replaces the M1 "temporary" path), `renderer.rs`
- `mf-desktop/src/viewport.rs`
- Test: `crates/mf-render/tests/incremental.rs`

**Interfaces:**
- **`Mesh` queries:**
  - `Mesh::face_chunk_shared(&self, other: &Mesh, i: usize) -> bool`
  - `Mesh::vertex_chunk_shared(&self, other: &Mesh, j: usize) -> bool`
- **`ChunkVertices`:** `MeshVertices` becomes `{ origin, chunks: Vec<Arc<ChunkVertices>>, lines }`, with one entry per face chunk.
  - `ChunkVertices { surface: Vec<SurfaceVertex>, vertex_chunks: Vec<u32> }`; `vertex_chunks` lists the vertex chunks its faces reference.
- **`MeshVertices::build_from(prev: Option<(&Mesh, &MeshVertices)>, next: &Mesh) -> MeshVertices`** runs on a worker.
  - A chunk is reused (the same `Arc`) iff its face chunk is shared **and** every vertex chunk in `vertex_chunks` is shared.
  - Otherwise it's rebuilt.
  - The origin is kept from `prev`, so reused chunks stay valid. It's recomputed only if the bounds grow by more than 10³ × the diagonal, for f32 precision.
- **`GpuMesh::sync(&mut self, device, queue, v: &MeshVertices) -> UploadStats`** uploads exactly the chunks whose `Arc` pointer differs from what is on the GPU, and drops chunks past the new count.
  - `UploadStats { surface_bytes: u64, state_bytes: u64, line_bytes: u64 }`.
  - `GpuMesh::total_uploaded()` exposes a cumulative counter (for the metrics panel and tests).

- [ ] **Step 1: Tests**
  - `filling_one_hole_reuploads_only_touched_chunks`: on a 1M mesh, `surface_bytes` ≤ 2 chunks plus the appended chunk.
  - `selection_change_uploads_no_geometry` (with Task 6)
  - `incremental_upload_matches_full_rebuild_after_checkout` (Review Focus 2): A → B → undo → branch C → checkout B. After each step, the GPU-side data equals `build_from(None, …)`. Compared through a CPU mirror of the uploaded chunks.
  - `moved_vertex_invalidates_every_chunk_that_references_it`
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(render): per-chunk GPU geometry with partial re-upload [MF-RENDER-003]`.

### Task 6: Per-face state buffer, selection tint and diagnostic overlays (MF-RENDER-003 selection, MF-RENDER-005)

**Files:**
- `mf-render`: `gpu_mesh.rs`, `shader.wgsl`, `renderer.rs`
- `mf-desktop`: `app.rs`, `panels.rs`
- Test: `crates/mf-render/tests/face_state.rs`, `crates/mf-desktop/tests/overlays.rs`

**Interfaces:**
- **`FaceStates`:** per face chunk, a `Vec<u32>` packing 4 faces per word (one byte per face).
  - Bits: `SELECTED = 1`, `SMALL_COMPONENT = 2`, `HOVER = 4`, `NON_MANIFOLD_ADJ = 8`.
  - Built on a worker from the selection and the analysis.
- **`GpuMesh::set_face_states(&mut self, queue, s: &FaceStates) -> UploadStats`** writes only the chunks whose words changed. The shader reads it as a storage buffer, indexed by `vertex_index / 3`.
- **Overlays:**
  - Non-manifold edges: an orange line overlay.
  - Small components (below the `remove_small_components` threshold): an amber tint.
  - Selected: cyan tint.
  - Hover: a light outline.
  - Each overlay can be toggled, with a legend in the side panel.
  - Clicking a non-manifold edge selects its incident faces (a `Faces` selection). Clicking a tinted island selects its component.

- [ ] **Step 1: Tests**
  - `selection_change_uploads_no_geometry`: `surface_bytes == 0` and `state_bytes > 0`.
  - `state_bits_tint_the_right_faces`: offscreen render; the pixel colour at the selected face's projection is cyan-dominant.
  - GUI `clicking_a_non_manifold_edge_selects_its_faces`
  - GUI `small_components_are_tinted_and_clickable`
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(render, desktop): face state buffer; selection, hover and defect overlays [MF-RENDER-003, MF-RENDER-005]`.

### Task 7: Accelerated picking and hover (MF-RENDER-004)

**Files:** `mf-render/src/bvh.rs` (new), `pick.rs`; `mf-desktop/src/{app.rs, worker.rs}`; `Cargo.toml` (`mf-render` at opt-level 2 in dev)

**Interfaces:**
- **`Bvh::build(mesh: &Mesh, ctx: &RunContext) -> Result<Bvh, Cancelled>`**
  - Binned-SAH build over live faces, with leaves of at most 8 faces. Memory is ≤ 64 B/face.
  - Built on the pool when a revision becomes current, and cached by `Mesh::ptr_eq`.
- **`Bvh::ray_pick(&self, mesh, origin, dir, model: &DMat4) -> Option<(FaceId, f64)>`** and **`Bvh::occluded(&self, mesh, from, to, skip: impl Fn(&[u32;3]) -> bool) -> bool`**, which `pick_loop` uses for its visibility test.
  - Brute force remains only for tests and as an oracle.
- **Hover:** pointer motion over the viewport issues at most one pick in flight. The newest position wins and stale results are dropped. The result sets the `HOVER` state bits on the loop's adjacent faces or the component, and a tooltip shows a hole's edge count and perimeter, or an island's triangle count.

- [ ] **Step 1: Tests**
  - `bvh_matches_brute_force_on_random_rays`: 10k rays, same face and distance.
  - `face_pick_under_16ms_at_5m`: median of 50 picks, release-optimised.
  - `bvh_build_is_cancellable`
  - GUI `hovering_a_hole_highlights_it_and_shows_its_size`
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(render): BVH picking and hover feedback [MF-RENDER-004]`.

### Task 8: Memory: topology scratch, budget gate, compaction (MF-GEO-002/003, MF-ISSUE-019/020)

**Files:**
- `mf-geometry/src/topology.rs`, `diagnostics.rs`
- `mf-operations/src/compact.rs`, `specs/operations/mesh.compact.yaml`
- `mf-cli` (`bench memory`)
- CI

**Interfaces:**
- **Topology build:** packed `u64` edge keys `(min << 32) | max` sorted in place, with no separate fan array. Peak at 5M is ≤ 150 MB (measured with `--metrics`).
- **`meshfixer bench memory --triangles N --json`** reports retained bytes (mesh plus topology) and the peak, with the counting allocator installed. CI gate: 5M ≤ 400 MB.
- **`mesh.compact`:** whole-object only, `effects: [renumbers_elements]`. It emits `Provenance.remap` (slot remap) like any edit, so selections translate through it.
- **Diagnostics:** gains `dead_slot_ratio`. The desktop suggests compaction above 0.5.

- [ ] **Step 1: Tests**
  - `topology_build_peak_under_150mb_at_5m`
  - `working_set_under_400mb_at_5m` (the CI job)
  - `compact_remaps_and_selections_translate`
  - `compact_is_replayable_exact`
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `perf(geometry): leaner topology build; memory gate; mesh.compact [MF-GEO-002, MF-ISSUE-019, MF-ISSUE-020]`.

### Task 9: Telemetry, metrics panel and orbit benchmarks (MF-RT-005, MF-RENDER-002 preliminary)

**Files:** `mf-metrics` (frame-time histogram), `mf-desktop/src/panels.rs` (Metrics panel), `mf-cli` (`bench orbit`), `scripts/bench-orbit.sh`

**Interfaces:**
- **`FrameTimes`:** a ring buffer of the last 1000 frames. `p50/p95/p99() -> Duration`.
- **Metrics panel (View ▸ Metrics):**
  - frame times p50/p95/p99, and fps
  - per-operation durations
  - peak heap
  - GPU buffer bytes (geometry, state, lines) and bytes uploaded on the last change
  - the thread budget with its `PoolChoice.reason`
- **`meshfixer bench orbit --triangles N --frames 600 --size 1920x1080 --json`:**
  - Runs offscreen on the damaged fixture, doing a full 360° orbit, with the GPU timed by `queue.on_submitted_work_done`.
  - Device info: CPU model and cores, GPU adapter name, backend and driver, RAM, OS, resolution (via `sysinfo` and the wgpu `AdapterInfo`).
  - `--metrics` JSON gains `gpu_buffer_bytes` and the frame-time percentiles.
- **`scripts/bench-orbit.sh`** runs 1M/5M/10M into `artifacts/m4/bench/` with fixtures in `mktemp -d`, then deletes them. A 30M measurement is recorded too, as information only.

- [ ] **Step 1: Tests**
  - `frame_time_percentiles` (unit test)
  - `bench_orbit_json_has_device_info_and_percentiles` (1M, CI lavapipe; a smoke test, not a gate)
  - `metrics_panel_shows_gpu_bytes_and_thread_budget` (GUI)
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Run `scripts/bench-orbit.sh` on the M2 Max and record the results as **preliminary** (OD-2 / MF-ISSUE-017 stay open).
- [ ] **Step 6:** Commit `feat(metrics): frame-time telemetry, metrics panel, orbit benchmark [MF-RT-005, MF-RENDER-002]`.

### Task 10: Hostile input: fuzzing and bounded readers (MF-IO-002)

**Files:** `fuzz/{Cargo.toml, fuzz_targets/{stl.rs, obj.rs, threemf.rs}}`, `fuzz/corpus/*` (small seeds only), `mf-io` (limits), CI (`fuzz` job)

**Interfaces:**
- **Reader limits** live in `mf_io::Limits { max_triangles: 50_000_000, max_model_part_bytes: 4 GiB, max_compression_ratio: 200 }`, with the defaults shown. Exceeding one is `IoError::LimitExceeded { what, limit, found }`, raised *before* allocating.
- **CI:**
  - A `fuzz` job (nightly, Linux) runs each target for 60 s on PRs and 15 min on a weekly schedule.
  - Crashes are uploaded as artifacts and fail the job.
  - The corpus is seeded from the generated fixtures.

- [ ] **Step 1: Tests (stable)**
  - `threemf_decompression_is_bounded` (Review Focus 5)
  - `stl_declaring_4_billion_triangles_fails_fast`
  - `obj_with_huge_indices_fails_fast`
  - `truncated_inputs_never_panic` (every prefix of each seed file)
- [ ] **Step 2–4:** FAIL → implement → PASS. Then run each fuzz target locally for 10 minutes; fix and add a regression test for every crash found.
- [ ] **Step 5:** Commit `test(io): fuzz targets per reader; bounded decompression and declared sizes [MF-IO-002]`.

### Task 11: Interaction quality: defect browser, framing, units (MF-RENDER-005 UX, MF-ISSUE-041/045)

**Files:** `mf-desktop/src/{panels.rs, app.rs, import_dialog.rs (new)}`, `mf-render/src/camera.rs`, `mf-document`, `mf-io`, `mf-cli`

**Interfaces:**
- **Defect browser** (side panel, "Defects"):
  - Holes, sorted by edge count, show size and perimeter in document units. Holes over 1000 edges are labelled "needs a large-hole method (not yet available)" (MF-ISSUE-043). Slits are labelled "slit: needs welding (not yet available)" (MF-ISSUE-032).
  - Islands, sorted by triangle count.
  - Non-manifold edges.
  - Clicking a row selects the defect and frames it.
  - Each row has a one-click action where one exists: "Fill", or "Remove island".
- **`OrbitCamera::frame(&mut self, aabb: &Aabb)`** animates over 200 ms (the animation interpolates the camera only; no geometry work).
- **Shortcuts:** `F` frames the selection, or everything if nothing is selected. `Esc` cancels a running job, otherwise clears the selection, and only when no popup or text field consumed it (MF-ISSUE-041). `Ctrl/Cmd+Z` and `Shift+Ctrl/Cmd+Z` are undo and redo.
- **Unit prompt (MF-ISSUE-045):**
  - On importing STL or OBJ, a dialog shows the bounding size as it would read in mm, m, cm and inch. It preselects the unit that gives a size between 10 mm and 2 m, or millimetres.
  - `Document` records the chosen unit, and 3MF export writes it.
  - The CLI takes `--units mm|cm|m|in`. Without the flag, a unitless input is assumed to be mm, with a warning that says so.
- **Copy:** every control's tooltip comes from its spec `description`. Every error message ends with what the user can do next. Empty states explain how to start ("Open a mesh (⌘O) or drop a file here").

- [ ] **Step 1: Tests**
  - GUI `defect_list_click_selects_and_frames_the_hole`
  - GUI `one_click_fill_from_defect_list_fills_only_that_hole`
  - GUI `esc_in_a_popup_keeps_the_selection`
  - GUI `unit_prompt_preselects_metres_for_the_archaeopteryx_scale` (a 0.5 × 0.4 m bounding box)
  - `exported_3mf_carries_the_chosen_unit`
  - `every_operation_parameter_has_a_tooltip`
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(desktop): defect browser, framing, shortcuts and unit prompt [MF-RENDER-005, MF-ISSUE-041, MF-ISSUE-045]`.

### Task 12: I/O burndown (MF-ISSUE-033/034/035/039)

**Files:** `mf-io/src/{lib.rs, threemf.rs}`, `mf-desktop` (export warnings)

**Behaviour:**
- **033:** 3MF export verification compares triangle indices and object names.
- **034:** empty objects are left out of the export, with a warning naming them.
- **035:**
  - STL/OBJ export applies each object's transform, with a status note.
  - `write_path(ThreeMf)` takes the document unit.
- **039:** only a *missing* `_rels/.rels` falls back to the standard path. A present but unreadable one is `IoError::Parse` naming it.

- [ ] **Step 1: Tests:** one per issue, named after its resolution criterion.
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `fix(io): export verification, empty objects, transforms/units on export, corrupt rels [MF-ISSUE-033, 034, 035, 039]`.

### Task 13: Operations and replay burndown (MF-ISSUE-036/037/038/040/046)

**Files:** `mf-selection`, `mf-operations/src/fill_holes/{mod.rs, refine.rs}`, `mf-command`, CI (`replay-cross-os`)

**Behaviour:**
- **036:** `TranslateError::LoopRemoved` is distinct from `LoopClosed { faces }`. The UI message names which.
- **037:** refinement rejects a flip that reverses a triangle normal relative to the patch.
- **038:** chords used by earlier loops in the same run join the excluded set.
- **040:** tests for `reexecute` cursor restore, `OperationVersion`, `UnknownRevision`, and orientation consistency after the real-scan fill.
- **046:** an integration test replays a committed log fixture recorded with a perturbed-position build flag (`MF_TEST_POSITION_JITTER=1e-9`). It expects `Equivalent`, and the same log with one changed index expects `Mismatch`. The CI cross-OS job prints Exact vs Equivalent counts per OS.

- [ ] **Step 1: Tests:** one per issue, named after its resolution criterion.
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `fix(ops, replay): loop-removed vs closed, fold-safe flips, shared chords, replay Equivalent path [MF-ISSUE-036, 037, 038, 040, 046]`.

### Task 14: Evidence, CI, independent review

- **Evidence job additions:**
  - the 1M orbit smoke JSON
  - memory gate JSON
  - cancel-timing JSON
  - GUI frames: progress bar mid-run, metrics panel, defect browser, two transformed objects
- **Independent whole-branch review** on the most capable model. The brief gets this plan's Review Focus verbatim, plus an explicit challenge to look for shortcuts:
  - a full re-upload disguised as incremental
  - cancellation checked only between phases
  - transforms applied in rendering but not in picking
  - frame-time numbers that exclude the GPU
- **Fix pass:** Critical and Important findings are fixed, each with a test that fails first. Minors go to `docs/issues.md`.
- [ ] Commit `ci: M4 evidence`. Then open the PR, with CI green on 3 OSes and 0 Linux skips.

### Task 15: UI/UX demonstration for the project owner, and close-out

**Files:** `docs/milestones/m4-demo.md` (script), `artifacts/m4/demo/` (inputs), `docs/milestones/m4.md`

**Deliverables:**
- **Demo build:** `cargo build --release -p mf-desktop`. The inputs are staged in `artifacts/m4/demo/`:
  - the 5M damaged sphere (generated)
  - the real Archaeopteryx scan
  - the two-object transformed 3MF
- **Demo script (`m4-demo.md`)**, about 15 minutes. Each step lists what to look for.
  1. **Performance (spec M4 demo):** open 5M and orbit, which should stay smooth. Run `fill_holes` and keep orbiting while progress moves. Cancel: the status says the document is unchanged, and the history is unchanged. Run again to completion. Open the Metrics panel.
  2. **Surgical repair (exit criterion 9, closing MF-ISSUE-015):**
     - Open the real scan. The unit prompt preselects metres.
     - Browse defects, hover a hole to see its size, click to frame it, and fill only that hole.
     - Undo, switch refinement, and fill again. Then try a slit: it's refused, with a clear reason.
     - Remove an island from the list.
     - Export 3MF, and check its true size in Cura.
  3. **Multi-object:** open the transformed 3MF. Both objects sit where they belong; click one, select a hole on it, and fill it.
- **Automated preview reel:** an MP4 of real GUI frames from the headless tests, assembled with ffmpeg and clearly labelled "automated preview, not the human demo". It's sent ahead, so the owner sees the flow before the live session.
- **Live session:** the owner drives with the script while it's recorded. Alternatively, Claude drives through computer use, if the owner enables it and grants access to the MeshFixer app. The recording is linked from `m4.md`, and MF-ISSUE-015 is closed with it.
- **UX feedback:** every friction point the owner notes becomes an issue with a severity. Anything S1/S2 is fixed before M4 is marked accepted, or the owner explicitly defers it.
- [ ] Write `docs/milestones/m4.md`: gates with evidence, benchmark tables marked preliminary, review findings, rulings, and the demo outcome. Commit `docs: M4 report and demo`.
