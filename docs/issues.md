# MeshFixer issue register

Every deferred review finding, spec gap and open milestone item is listed here until it's resolved. "Milestone" is the latest point by which it must be closed. Closed items stay listed, with the commit or test that closed them.

Severity: **S1** data loss or wrong geometry · **S2** wrong or misleading result, crash on unusual input · **S3** UX, diagnostics or robustness polish.

## Open

| ID | Sev | Title | Source | Milestone | Resolution criteria |
|---|---|---|---|---|---|
| MF-ISSUE-019 | S3 | Topology construction scratch is 468 MB at 4.8M triangles | ADR-0001 measurements | M4 | Topology build peak ≤ 150 MB at 5M (packed `u64` edge keys, no separate fan array), measured with `--metrics`. |
| MF-ISSUE-020 | S3 | Tombstoned slots accumulate in long sessions | ADR-0001 | M4 | A `mesh.compact` operation that emits provenance (a slot remap) like any edit, plus a dead-slot ratio in diagnostics. |
| MF-ISSUE-032 | S2 | Slits (collinear cracks) can't be closed | M3 review I4 | M4 (roadmap: weld/stitch/zipper gap repair, a core real-scan capability) | 6 of the real scan's 101 holes are slits: their vertices are collinear, so any fill would be zero-area triangles. `fill_holes` now refuses them by name (`untriangulable`). Closing them needs a zip/weld operation that merges the crack's edges, with provenance. |
| MF-ISSUE-033 | S3 | 3MF export verification doesn't compare triangle indices or names | M3 review minor | M4 | `write_model_path` checks indices and object names, not just face counts and positions. |
| MF-ISSUE-034 | S3 | An empty mesh object is written as an invalid 3MF object | M3 review minor | M4 | Empty objects are left out of the export with a warning, or the export fails by name. The core schema needs at least 3 vertices and 1 triangle. |
| MF-ISSUE-035 | S2 | STL/OBJ export ignores object transforms and 3MF export of a single mesh ignores document units, without saying so | M3 review minor | M4 | Export either applies the transform or warns that it was dropped. `write_path(ThreeMf)` takes the document's unit. |
| MF-ISSUE-036 | S3 | A loop removed with its island reports "Hole filled (0 faces)" | M3 review minor | M4 | Translation distinguishes a loop closed by faces from one whose surroundings were removed, and the UI message says which. |
| MF-ISSUE-037 | S3 | Refinement flips have no fold check on non-planar patches | M3 review minor | M4 | A flip is rejected if it reverses a triangle normal relative to the patch, with a test on a saddle-shaped hole. |
| MF-ISSUE-038 | S3 | Two loops sharing two non-adjacent vertices could both use that chord in one whole-object run | M3 review minor (theoretical) | M4 | Chords chosen for earlier loops in the same run are added to the excluded set, with a test. |
| MF-ISSUE-039 | S3 | A corrupt `_rels/.rels` is treated as missing (warning only) | M3 review minor | M4 | Only a missing entry falls back to the standard path. A present but unreadable `.rels` is an error. |
| MF-ISSUE-040 | S3 | Test gaps: `reexecute` cursor restore, `OperationVersion` and `UnknownRevision`; orientation after the real-scan fill | M3 review minor / executor | M4 | Tests for each. |
| MF-ISSUE-041 | S3 | Esc closing a popup also clears the selection | M3 review minor | M4 | Esc clears the selection only when no popup consumed it. |
| MF-ISSUE-042 | S2 | Object transforms aren't applied in the viewport or in picking | M3 sign-off condition | **M4, early (mandatory)** | Multi-object documents render every object at its transform, and picking, overlays and selection work in transformed space. Tested with a 3MF of transformed objects. Until this is done, multi-object editing isn't called supported. |
| MF-ISSUE-043 | S2 | Holes over 1000 edges can't be filled | M3 sign-off condition | M5 | The 1000-edge cap belongs to the current O(n³) DP, not to MeshFixer. Large holes go to a scalable strategy (polygon decomposition, constrained or advancing-front triangulation), chosen automatically by geometry and intent, not exposed as an algorithm limit to the user. |
| MF-ISSUE-044 | S2 | Slicer import is checked, not printability | M3 sign-off condition | M5 | An automated slicing test (for example CuraEngine headless) produces layers. It also checks expected physical dimensions and scale, and the geometry checks a printer needs (closed, manifold, no degenerate faces). |
| MF-ISSUE-045 | S2 | OBJ has no units, so a metre-scale scan imports at millimetre size | M3 Cura check | M4 | On import, the user can set (or MeshFixer detects and confirms) the unit of a unitless file, and the document records it. 3MF export writes the right unit. |
| MF-ISSUE-046 | S3 | The `Equivalent` replay verdict has no integration-level test | M3 sign-off condition | M4 | An end-to-end test replays a log whose positions legitimately differ (for example, a recorded log from another build or platform), gets `Equivalent`, and still detects a real `Mismatch`. Exact and Equivalent stay distinct in reports. |
| MF-ISSUE-015 | S3 | No human-driven demonstration recording | M1 evidence | M1 sign-off | A screen recording of open → inspect → apply → undo → redo → export on a real machine, linked from `docs/milestones/m1.md`. |
| MF-ISSUE-016 | S3 | Font licence texts must ship with binaries | Font exception | First binary release | Installer or About dialog includes the texts in `licenses/third-party/`. See `THIRD_PARTY_NOTICES.md`. |
| MF-ISSUE-017 | S2 | Reference integrated-GPU hardware not available | Spec OD-2 | Before MF-RENDER-002 is declared met | Benchmarks run on a named 16 GB integrated-GPU laptop, with device info recorded. |
| MF-ISSUE-018 | S3 | Orientation repair not offered | Review of volume status | M3+ | `volume_status = inconsistent_orientation` gets a `mesh.orient_faces` operation (targetable). Until then, diagnostics distinguish the cause (done). |

## Closed

| ID | Title | Closed by |
|---|---|---|
| MF-ISSUE-001 | Counts beyond `u32` are truncated silently | `Mesh::check_counts` in `Mesh::new` and `MeshEdit::finish` · test `new_rejects_counts_over_u32` (M2 T3) |
| MF-ISSUE-003 | Non-finite diagnostic values serialize as `null` | `non_finite` flag, `VolumeStatus::NonFinite`, `Option` area and perimeter · test `huge_coordinates_report_non_finite` (M2 T5) |
| MF-ISSUE-004 | Swallowed errors | CLI JSON error propagates; all import warnings listed; topology `unwrap_or(0)` removed (corners derived from half-edges); invariants use `expect`; CI `scripts/check-swallowed-defaults.sh` with a reviewed allowlist (M2 T3/T10) |
| MF-ISSUE-005 | No-op operation still creates a revision and a full copy | `OpOutput.change = None`, `CommitOutcome::NoChange` · tests `noop_returns_no_change`, `noop_operation_creates_no_revision` (M2 T6) |
| MF-ISSUE-006 | Empty mesh shows "Open a mesh" | Viewport placeholder "Empty mesh (0 triangles)" (M2 T10) |
| MF-ISSUE-007 | Touching boundary loops (two holes sharing a vertex) aren't traced | Edge-based loop tracing with counter-clockwise gap pairing at pinched vertices · tests `bowtie_holes_trace_as_two_loops`, `three_loops_meeting_at_one_vertex` (M2 T5) |
| MF-ISSUE-008 | Doubled triangle reports closed and manifold | `duplicate_faces` · test `duplicate_face_counted` (M2 T5) |
| MF-ISSUE-009 | Keyboard and lifecycle UX | Shortcuts yield to focused text fields; Edit menu disabled while busy; GPU buffers released with no active object (M2 T10) |
| MF-ISSUE-011 | Ledger evidence recorded the empty doc-test line | Ledger records summed totals; CI summary script (M2 T1) |
| MF-ISSUE-012 | **Spec gap:** tolerance duplicate vertices (MF-GEO-004) not implemented | `duplicate_vertices_tolerance` (grid hash, 1e-6 × diagonal) · test `near_duplicates_counted` (M2 T5) |
| MF-ISSUE-013 | Memory: 1.1 GB peak RSS at 4.8M triangles vs 400 MB budget | Counting allocator + `--metrics`; 4.8M working set 143 MB (budget 400); process peak 1383 → 612 MB. See ADR-0001 (M2 T1–T3) |
| MF-ISSUE-030 | Real scan fixture licence not confirmed at record level | The project owner checked the official 3D record (package `391660da-…`, the same package the fixture is fetched from) on 2026-11-08: the 3D media is public domain, metadata CC0. Evidence and a courtesy attribution are in `fixtures/SOURCES.md` |
| MF-ISSUE-025 | Cross-OS replay never exercised a computed-position path | The cross-OS log includes a refined `fill_holes`. CI run 42529263446 replayed it as **Exact** on macOS and Windows: refined positions are bit-identical across OSes, so the `Equivalent` path is still only covered by unit tests (`equivalent_when_only_positions_differ`) |
| MF-ISSUE-027 | `Invocation` had no selection field | `Invocation.selection`, checked against the current revision (`SelectionMismatch`/`StaleSelection`) and stored in the record · tests in `selection_commands.rs` |
| MF-ISSUE-031 | 3MF reading was quadratic in file size (an 89k-triangle file took 554 s; exporting 3MF re-reads to verify, so exports hung too) | Found while producing M3 evidence: each XML event's line number was found by counting newlines from the start of the file. A `LineCounter` now counts only since the previous event · test `threemf_read_is_linear_in_file_size` (RED: 554 s → GREEN well under its 5 s limit) |
| MF-ISSUE-029 | fill_holes could add a face to an existing edge (non-manifold result on a real scan) | Found by the real scan fixture: loops 70 and 92 of the Archaeopteryx scan triangulated with a chord that was already a mesh edge outside the hole. The DP now excludes chords between loop vertices already joined by an edge. If none remain, the loop is skipped (`untriangulable`), or rejected when explicitly selected (`loop_untriangulable`) · tests `a_chord_that_is_already_a_mesh_edge_is_never_used`, `no_triangulation_when_every_chord_exists`, `fill_each_small_hole_of_real_scan_untouched_outside` (all RED without the fix) |
| MF-ISSUE-022 | Swallowed-error check is narrow | Checker flags `unwrap_or_default()`, `unwrap_or(0)`, `.ok()` and `let _ =` unless the line (or the one above) carries `// allow-swallow: <reason>`; per-line, so duplicates can't slip through; 19 sites reviewed and annotated; the checker has its own fixture tests (`scripts/test-check-swallowed.sh`) |
| MF-ISSUE-024 | Offscreen device cache keeps an error or lost device for the process lifetime | `offscreen::shared_device` caches only live devices: creation errors are returned and retried, and a device reported lost (callback or failed poll) is replaced · test `device_loss.rs::render_recovers_after_the_shared_device_is_lost` |
| MF-ISSUE-028 | Near-duplicate detection holds a per-vertex hash grid (inspect peak 701 MB at 4.8M vs 577 MB for an edit) | Sorted-array grid (~32 B/point) instead of a hash map of vectors; sort buffers scoped · inspect peak 204 → 47 B/vertex · test `inspect_peak_per_vertex_is_bounded` |
| MF-ISSUE-026 | Equivalence grid degrades to O(n²) with tiny tolerance and huge coordinates | Shared compact `PointGrid` keyed relative to the bounding box with clamped cell size; also fixed an i64 overflow panic in near-duplicate detection far from the origin · tests `equivalence_stays_fast_with_offset_coordinates_and_tiny_tolerance`, `inspect_far_from_origin_does_not_overflow` (RED on old code) |
| MF-ISSUE-021 | Misleading replay/log error variants | Missing ancestor is an invariant (`expect`); a missing replay target is named (`does not exist` / `LogError::TargetMissing`) · test `replay_names_a_missing_target_object` |
| MF-ISSUE-002 | Misleading STL error messages | STL import returns warnings: header count 0 read by size, trailing bytes ignored (both warned); short files are errors naming the ambiguity; solid-header mismatch reported as such · tests in `io.rs::stl_header_cases` |
| MF-ISSUE-023 | Created-then-removed face can remain a `Sources` entry of other created faces | Sources must be live faces of the input revision (`GeometryError::InvalidSource`) · test `sources_must_be_faces_of_the_input_revision` |
| MF-ISSUE-014 | Cross-platform CI not yet run | CI run 42492436343 (PR #1): ubuntu, macOS and Windows each 84 verified passes, 0 skipped, 0 failed. Every GPU and GUI test ran on all three (lavapipe, Metal, WARP). |
| MF-ISSUE-010 | Non-atomic export could destroy the source file | `fix(io): atomic, verified export` · tests `failed_export_leaves_existing_file_intact`, `export_replaces_existing_file_atomically` |
| — | Binary STL export wrote `inf` for out-of-range coordinates (part of minor 3) | Same commit: `write_binary` rejects coordinates outside f32 |
| — | Volume hidden without saying why | `feat(geometry): volume_status` · test `volume_status_explains_missing_volume` |
| — | M1 review C1, I1, I2, I3 | See `docs/milestones/m1.md` |
