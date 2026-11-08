# MeshFixer issue register

Every deferred review finding, spec gap and open milestone item is listed here until it's resolved. "Milestone" is the latest point by which it must be closed. Closed items stay listed, with the commit or test that closed them.

Severity: **S1** data loss or wrong geometry · **S2** wrong or misleading result, crash on unusual input · **S3** UX, diagnostics or robustness polish.

## Open

| ID | Sev | Title | Source | Milestone | Resolution criteria |
|---|---|---|---|---|---|
| MF-ISSUE-019 | S3 | Topology construction scratch is 468 MB at 4.8M triangles | ADR-0001 measurements | M4 | Topology build peak ≤ 150 MB at 5M (packed `u64` edge keys, no separate fan array), measured with `--metrics`. |
| MF-ISSUE-020 | S3 | Tombstoned slots accumulate in long sessions | ADR-0001 | M4 | A `mesh.compact` operation that emits provenance (a slot remap) like any edit, plus a dead-slot ratio in diagnostics. |
| MF-ISSUE-025 | S2 | Cross-OS replay never exercises the `Equivalent` path | M2 review minor | M3 | The CI cross-OS job includes an operation that computes positions (fill_holes refinement). Its summary shows Exact vs Equivalent per OS. |
| MF-ISSUE-027 | S3 | `Invocation` has no selection field yet | M2 review recommendation | M3 Task 1 | Frontends pass a `Selection`, which is converted to a `Target` and stored in the record. The log format is unchanged (records already carry `selection`). |
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
