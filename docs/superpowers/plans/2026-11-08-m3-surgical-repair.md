# M3 Real, Surgical Repair Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Click one hole on a real scan, fill only that hole, inspect it, undo, change a parameter, fill again, and export 3MF that opens in a slicer. That's spec exit criterion 9.

**Architecture:** Selections become a first-class part of a command. A `Selection` bound to the current revision travels in the `Invocation`, is checked against the operation spec's target kinds, becomes the operation's `Target`, and is stored in the command record. `mesh.fill_holes` triangulates each target loop (Liepa's minimum-weight DP) and refines the patch to the surrounding density, purely through `MeshEdit`. Kept geometry is therefore provably untouched, and provenance names the loop. The desktop picks loops in screen space and faces by ray cast on a worker, then translates the selection after every history move. A declarative YAML runner turns "given / when / expect" geometry cases into tests.

**Tech Stack:** As M2 (Rust 1.99.1), plus `zip` and `quick-xml` (MIT) for 3MF.

**Spec:** `docs/spec/phase0-spec.md` v0.2 (MF-OP-001/002/003, MF-SEL-003/004, MF-SPEC-004, MF-IO-001, MF-DOC-004, MF-HIST-002, exit criterion 9). Milestone: `docs/plan/phase0-milestones.md` § M3. Issues: MF-ISSUE-025, MF-ISSUE-027.

## Global Constraints

- Everything from M2's constraints: licence, toolchain, determinism, no silent fallbacks, `// allow-swallow: <reason>` for reviewed defaults, and `scripts/check.sh` green before every commit (`fmt && check.sh && git commit`, never `;`).
- **Never switch branches with uncommitted changes.**
- All geometry changes go through `MeshEdit`. No operation compacts or renumbers kept elements.
- `untouched_outside_target`: every face live in the input and outside the target keeps its ID, indices and vertex positions bit-for-bit (MF-OP-003).
- Hole filling never fan-fills and never skips a targeted loop silently (MF-OP-002).

## Review Focus

1. **Patch orientation:** the patch must wind opposite to the boundary half-edges, or the result is closed but non-orientable. Test: `filled_cube_is_closed_and_consistently_oriented` (Task 2).
2. **Loops through a pinch vertex:** filling one of two holes that share a vertex must leave the other loop open and traced. Test: `fill_one_pinched_hole_leaves_the_other` (Task 2).
3. **Selection after the edit that consumes it:** after filling a selected loop, translating that selection must give `Failed(LoopClosed)` naming the patch faces, and the UI must clear it with a message, not keep a dead ID. Tests: `selection_of_filled_loop_reports_loop_closed` (Task 1/2), GUI test (Task 6).
4. **Degenerate loops:** 3-edge loops (fill with exactly one triangle), collinear loops, and loops whose vertices repeat (pinched figure-8 boundaries). Each must either fill correctly or fail with a named error. Test: `degenerate_loops_fill_or_fail_by_name` (Task 2).
5. **3MF edge cases:** multiple objects, `build` items with transforms, a missing `unit` (default millimetre), and a model with zero triangles. Test: `threemf_edge_cases` (Task 5).

---

### Task 1: Selections in commands (MF-ISSUE-027, MF-SEL-004)

**Files:** `mf-command/src/session.rs`, `mf-operations/src/lib.rs`, `mf-spec` (target kind names), `mf-cli/src/main.rs`

**Interfaces (produces):**
- `Invocation { op_id, target: ObjectId, selection: Option<Selection>, raw_params }`. `None` means the whole object. Existing constructors use struct syntax, so every call site sets `selection: None`.
- `mf_operations::Target::from_selection(&Selection) -> Target`: `Object` becomes `WholeObject`, `Faces` becomes `Faces`, `Component` becomes `Components`, `BoundaryLoop` becomes `Loops`.
- `Session::prepare` checks, in order:
  - `selection.object == target` → else `CommandError::SelectionMismatch`
  - `selection.revision == current()` → else `CommandError::StaleSelection { selection, current }`. Callers translate first; there's no implicit translation.
  - The kind is allowed by the spec's `target.kinds`, or `whole_object` is true for `None` → else `CommandError::UnsupportedTarget { op, kind }`.
- `CommandRecord.selection` is filled from the invocation. Replay passes it through unchanged (revision IDs replay identically).
- CLI: `--select loop:<id>[,<id>]`, `--select component:<id>`, `--select faces:<id>,<id>` on operation subcommands and on `run` (it applies to the preceding `--op`). IDs refer to the imported revision.

- [ ] **Step 1: Tests** (`mf-command/tests/selection_commands.rs`)
  - `selection_is_recorded`
  - `stale_selection_is_rejected` (selection made at the import revision, after one edit → `StaleSelection`)
  - `selection_for_another_object_is_rejected`
  - `kind_not_in_spec_is_rejected` (`remove_small_components` with a `BoundaryLoop` selection → `UnsupportedTarget`)
  - CLI `select_arg_parses_and_reaches_the_record`
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Update `mesh.remove_small_components.yaml` to `target.kinds: [component]`. Implement the `Components` target: remove the selected components if they're under the threshold, ignoring `keep_largest`. A selected component at or above the threshold is an error naming it, never a silent no-op. Test `remove_selected_component_only`.
- [ ] **Step 6:** Commit `feat(command): selections travel in invocations and records [MF-ISSUE-027, MF-SEL-004]`.

### Task 2: `mesh.fill_holes`, triangulation (MF-OP-001/002/003)

**Files:** `specs/operations/mesh.fill_holes.yaml`, `mf-operations/src/fill_holes/{mod.rs, triangulate.rs}`, registry, builtin specs list

**Spec file:**
- `id: mesh.fill_holes`, `version: 1`, `target: { whole_object: true, kinds: [boundary_loop] }`, `effects: [geometry_changes, topology_changes]`.
- Parameters:
  - `max_loop_edges` (integer, default 500, min 3, max 5000; "Largest hole (in boundary edges) to fill")
  - `refine` (bool, default true; "Add vertices so the patch matches the surrounding triangle density")

**Behaviour:**
- **`Loops(ids)`:** every ID must be a traced loop (else `invalid_target`, naming it). Any loop with `edge_count > max_loop_edges` fails with `loop_too_large`, naming the loop and its size. All loops are validated before any are filled, so failure is all-or-nothing (MF-CMD-006).
- **`WholeObject`:** fills every loop with `edge_count ≤ max_loop_edges`. Larger loops are listed in the report as `skipped_loops: [{id, edges}]`. That's explicit, not silent: the outer rim of an open scan is usually one of them.
- **Triangulation:** Liepa 2003 minimum-weight DP over the loop polygon in **reverse** order, so the patch winds opposite to the boundary half-edges. The weight of triangle (i, m, j) is lexicographic `(max dihedral angle with its neighbours, area)`. Neighbours are the existing face across a boundary edge, or the adjacent DP triangle. O(n³) time, O(n²) memory.
- **Created faces:** `Sources { faces: [], loops: [loop id] }`.
- **Report:** `{filled_loops: [{id, edges, faces}], skipped_loops: [...], created_faces, created_vertices}`.

- [ ] **Step 1: Tests** (`mf-operations/tests/fill_holes.rs`)
  - `filled_cube_is_closed_and_consistently_oriented` (cube with a missing face → closed, manifold, 0 inconsistent edges, volume 1.0 ± 1e-12)
  - `fills_only_the_selected_loop` (two holes; fill one; the other loop is still traced)
  - `untouched_outside_target` (every input face kept with the same ID and indices; every input vertex position bitwise equal)
  - `fill_one_pinched_hole_leaves_the_other`
  - `loop_too_large_fails_by_name`
  - `whole_object_reports_skipped_loops`
  - `degenerate_loops_fill_or_fail_by_name`
  - `provenance_names_the_loop`
  - `selection_of_filled_loop_reports_loop_closed` (through `Session::translate`)
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(operations): fill_holes with minimum-weight triangulation, targeted or whole-object [MF-OP-001, MF-OP-002, MF-OP-003]`.

### Task 3: Patch refinement (MF-OP-002 quality)

**Files:** `mf-operations/src/fill_holes/refine.rs`

**Algorithm (Liepa 2003 §3):**
1. **Densities:** σ(v) for each boundary vertex is the mean length of its existing incident edges. Each new vertex gets the mean σ of the triangle it splits.
2. **Split:** repeat until nothing splits. For each patch triangle with centroid c, split at c (3 new faces) if, for every corner v, `√2·|c − v| > σ(c)` and `√2·|c − v| > σ(v)`.
3. **Relax:** after each split pass, flip interior patch edges whose opposite vertex lies inside the circumcircle (Delaunay-like), iterated to a fixpoint. Edges shared with kept geometry are never flipped. Flips use `MeshEdit::set_face`, so patch faces keep their IDs.

Positions come only from centroids of patch triangles, so everything stays in the patch's span. Fairing is a stretch goal and not part of this task.

- [ ] **Step 1: Tests**
  - `refined_patch_matches_boundary_density` (a 64-edge round hole in a fine sphere: mean patch edge length within ±25% of the mean boundary edge length; minimum angle ≥ 5°)
  - `refine_false_gives_the_bare_triangulation`
  - `refinement_is_deterministic_at_1_and_8_threads`
  - `untouched_outside_target` still passes with refinement on
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(operations): fill_holes refinement to the surrounding density [MF-OP-002]`.

### Task 4: Declarative geometry tests (MF-SPEC-004)

**Files:** `tests/geometry/*.yaml`, `crates/mf-operations/tests/declarative.rs` (runner), `specs/geometry-test.schema.json`

**Case format:**
```yaml
name: fill one hole of two
given: { fixture: two_holes_grid }   # or { file: fixtures/... }
select: { loop_containing_vertex: 9 }  # or component_containing_face, faces, whole object if omitted
when: { op: mesh.fill_holes, params: { refine: false } }
expect:
  boundary_loops: 1
  components: 1
  is_manifold: true
  untouched_outside_target: true
  provenance_total: true
```

- **Fixtures** come from a registry function in the runner (named generators from `mf-fixtures`).
- **Selectors** are resolved on the input mesh. Selectors use stable geometric anchors (a vertex or a face), never revision-scoped loop IDs.
- **Expect keys** are any numeric or boolean `Diagnostics` field, plus `volume_delta_pct_lt`, `untouched_outside_target` and `provenance_total`.
- **Failure output** names the file, the case and every mismatching key.

- [ ] **Step 1: Tests:** runner self-tests (`a_failing_case_reports_file_case_and_key`, `unknown_expect_key_is_an_error`, `cases_validate_against_schema`). Then 8–12 cases covering `remove_small_components` (object and component targets) and `fill_holes` (cube, two holes, pinched, too-large, refine on and off).
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `test: declarative YAML geometry tests [MF-SPEC-004]`.

### Task 5: 3MF read/write; transforms and units (MF-IO-001, MF-DOC-004)

**Files:** `mf-io/src/threemf.rs`, `mf-io/src/lib.rs`, `mf-document` (transform + units), `mf-command` (multi-object import), CLI

**Interfaces (produces):**
- `Object.transform: [f64; 12]`: a row-major 3×4 affine, identity by default. It's stored only, not applied to geometry in M3; the viewer draws the active object untransformed (recorded in the ruling).
- `Document.units: Units { Millimeter (default), Inch, Micron, Centimeter, Foot, Meter }`.
- `mf_io::threemf::{read(bytes) -> Result<Model, IoError>, write(&Model) -> Result<Vec<u8>, String>}` with `Model { unit, objects: Vec<ModelObject { name, mesh, transform }> }`.
  - Reads `_rels/.rels` to find the model part, `<object type="model">` meshes, `<build><item objectid transform>`, and the `unit` attribute.
  - Unsupported 3MF features (components, extensions) give a warning, never silent loss.
- `mf_io::read_model(path) -> Result<ImportedModel, IoError>` for every format. STL and OBJ give one object with an identity transform.
- `Session::import_model(ImportedModel) -> Vec<ObjectId>` makes one revision.
- Writes are atomic and verified as in M1, with `Format::ThreeMf`.

- [ ] **Step 1: Tests**
  - `threemf_roundtrip_three_objects` (names, transforms, unit)
  - `threemf_edge_cases` (missing unit means mm; an empty mesh; a non-identity transform; an unsupported `<components>` gives a warning)
  - `threemf_rejects_malformed_xml_with_location`
  - `export_3mf_skips_tombstones`
  - CLI `inspect` and `run` on 3MF
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(io): 3MF import/export with objects, transforms and units [MF-IO-001, MF-DOC-004]`.

### Task 6: Picking and selection in the desktop (MF-RENDER-004/005 for M3, exit criterion 9)

**Files:** `mf-render/src/{pick.rs, gpu_mesh.rs, renderer.rs, shader.wgsl}`, `mf-desktop/src/{app.rs, panels.rs, viewport.rs}`

**Behaviour:**
- **Loop pick** (UI thread; loops are small): project each traced loop's edges with the view-projection and choose the nearest edge within 8 px of the click.
- **Face pick** (worker): CPU ray-triangle over live faces, choosing the nearest hit. The component is looked up from topology. Brute force is acceptable in M3 (M4 adds acceleration for the < 16 ms budget).
- **Click semantics:** a click selects a loop if one is within 8 px, otherwise the face's component. Shift-click adds to the selection. Esc clears it.
- **Highlight:** selected loops are drawn as a separate highlight line buffer in cyan (a new `fs_highlight` pipeline). A selection change uploads only this buffer (MF-RENDER-003).
- **Panel:** shows the current selection. The operation panel says which target Apply will use: the selection if the spec accepts its kind, otherwise the whole object, labelled.
- **After every history move** (commit, undo, redo, checkout): translate the selection to the new revision.
  - `Mapped` keeps it.
  - `Partial` keeps it and shows a status message.
  - `Failed(LoopClosed)` clears it with "Hole filled (N faces)".
  - Any other result clears it with the reason.

- [ ] **Step 1: Tests**
  - `mf-render`: `loop_pick_finds_nearest_edge_within_tolerance`, `ray_pick_finds_nearest_face`
  - GUI test `select_hole_fill_undo_refill` (open the two-holes fixture, pick a loop by projected coordinates, Apply `fill_holes`, check the other loop is still open, undo, change `refine`, Apply again, export 3MF)
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(desktop): pick holes and components, highlight, selection follows history [MF-SEL-003, exit criterion 9]`.

### Task 7: Real scan fixture

**Files:** `scripts/fetch-fixtures.sh`, `fixtures/SOURCES.md`, `fixtures/manifest.txt`, `mf-fixtures/src/real.rs`, CI

- **Find** a CC0 (preferred) or CC-BY scan of 0.5–5M triangles with real holes and islands, from the Smithsonian Open Access 3D collection (CC0) or a similarly licensed source.
- **Record** its URL, licence, author, SHA-256 and access date in `fixtures/SOURCES.md` and `fixtures/manifest.txt`.
- **Fetch** with `scripts/fetch-fixtures.sh` into `fixtures/real/` (git-ignored), verifying SHA-256. Any mismatch is a hard error.
- **Tests that need it** use `mf_fixtures::real::path("name")`. A missing file skips through the skip policy, and `MF_REQUIRE_FIXTURES=1` turns the skip into a failure. CI's Linux job and the evidence job set it after fetching (cached by hash).

- [ ] **Step 1:** Choose and verify the scan by downloading it, checking the licence text, and checking it has open boundary loops and islands via `meshfixer inspect`.
- [ ] **Step 2: Tests:** `real_scan_has_holes_and_islands`, `fill_each_small_hole_of_real_scan_untouched_outside` (every loop ≤ 200 edges, one command each, with `untouched_outside_target` after each), `real_scan_render_before_after` (offscreen PNGs into `MF_ARTIFACTS`).
- [ ] **Step 3:** Commit `test: real scan fixture fetched by hash; surgical fills on real data`.

### Task 8: Re-execution against a changed upstream (MF-HIST-002)

**Files:** `mf-command/src/session.rs`

**Interface:** `Session::reexecute(&mut self, record: &CommandRecord, onto: RevisionId) -> Result<CommitOutcome, CommandError>`. It translates `record.selection` to `onto` (`Mapped` or `Partial` proceed and anything else is an error naming the translation result), checks out `onto`, and runs the same operation and parameters. This is the primitive that parameter editing and downstream recompute will be built from.

- [ ] **Step 1: Tests**
  - `reexecute_fill_on_regenerated_upstream`: import, remove islands, select a loop, fill (record R). Then branch from the import with a different upstream (another `remove_small` threshold), re-execute R there, and check a valid fill of the translated loop.
  - `reexecute_fails_by_name_when_target_gone`
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(command): re-execute a recorded command on another revision [MF-HIST-002]`.

### Task 9: CI and evidence

- The cross-OS replay job also runs a `fill_holes` log with `refine: true`, so positions get computed (MF-ISSUE-025). The job summary shows the verdict per OS.
- The evidence job adds: real scan before/after renders, a filled-hole 3MF, a replay report with `fill_holes`, and GUI frames from `select_hole_fill_undo_refill`.
- [ ] Commit `ci: fill_holes in cross-OS replay; M3 evidence [MF-ISSUE-025]`.

### Task 10: Close-out

- [ ] `scripts/check.sh` green, open a PR, CI green on 3 OSes with 0 Linux skips.
- [ ] Manual check: open the exported 3MF in UltiMaker Cura, take a screenshot, and record it in the report.
- [ ] Independent whole-branch review (most capable model). The brief includes challenging shortcuts that pass tests without real surgical editing. One fix pass: Critical and Important findings fixed, with tests that fail first. Minor findings go into `docs/issues.md`.
- [ ] `docs/milestones/m3.md`, and close issues 025 and 027 in the register.
