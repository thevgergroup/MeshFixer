# MeshFixer issue register

Every deferred review finding, spec gap and open milestone item is listed here until it's resolved. "Milestone" is the latest point by which it must be closed. Closed items stay listed, with the commit or test that closed them.

Severity: **S1** data loss or wrong geometry · **S2** wrong or misleading result, crash on unusual input · **S3** UX, diagnostics or robustness polish.

## Open

| ID | Sev | Title | Source | Milestone | Resolution criteria |
|---|---|---|---|---|---|
| MF-ISSUE-001 | S2 | Counts beyond `u32` are truncated silently | M1 review minor 1 | M2 | `Mesh::new` rejects `positions.len() > u32::MAX` and `triangles.len()*3 > u32::MAX`, and `stl::read` uses `checked_mul` on the facet count before allocating. Tests use synthetic counts, not real allocations. |
| MF-ISSUE-002 | S3 | Misleading STL error messages | M1 review minor 2 | M2 | A binary file with a wrong facet count or trailing padding gets either a warning-and-read-by-size path or an error that names the binary/ASCII ambiguity. A `solid` header with a size mismatch reports "binary STL with ASCII-looking header", not a UTF-8 error. A fixture covers each case. |
| MF-ISSUE-003 | S2 | Non-finite diagnostic values serialize as `null` | M1 review minor 3 (export part fixed) | M2 | Area, perimeter and volume overflow is reported explicitly (an error or `non_finite` status), never a silent `null`. Test with coordinates near 1e308. |
| MF-ISSUE-004 | S2 | Swallowed errors | M1 review minor 4 | M2 | The CLI JSON serialization error propagates (exit 1). All import warnings are shown, not just the last. `topology.rs` `unwrap_or(0)` and the viewport's `unwrap_or_default()` are replaced by invariants that fail loudly. A grep check for `unwrap_or_default` in non-test code goes into CI with an allowlist. |
| MF-ISSUE-005 | S3 | No-op operation still creates a revision and a full copy | M1 review minor 5 | M2 | When `remove_small_components` removes nothing, it returns a report with no new revision (or a revision sharing storage), and unreferenced vertices are kept. Test: no history entry and pointer-equal mesh. |
| MF-ISSUE-006 | S3 | Empty mesh shows "Open a mesh" | M1 review minor 6 | M2 | The viewport shows "Empty mesh (0 triangles)" for an opened empty object. Covered by the GUI test. |
| MF-ISSUE-007 | **S1** | Touching boundary loops (two holes sharing a vertex) aren't traced | M1 review minor 7 | **Before M3 starts** | Loops are split at pinch vertices into separate traced loops with stable IDs. `untraced_boundary_edges` is 0 for bowtie-hole fixtures. Required before `fill_holes` (MF-OP-002), because filling an untraced loop is impossible and filling a merged loop gives wrong geometry. |
| MF-ISSUE-008 | S3 | Doubled triangle reports closed and manifold | M1 review minor 8 | M2 | Add a `duplicate_faces` count (same vertex set) to diagnostics, with a fixture. |
| MF-ISSUE-009 | S3 | Keyboard and lifecycle UX | M1 review minor 9 | M2 | Cmd+Z goes to a focused text field before document undo. The Edit menu's Undo/Redo are disabled while busy. GPU buffers are released when the active object disappears. |
| MF-ISSUE-011 | S3 | Ledger evidence recorded the empty doc-test line | M1 review minor 11 | M2 process | Task completion records summed test totals (CI summary script). |
| MF-ISSUE-012 | S2 | **Spec gap:** tolerance duplicate vertices (MF-GEO-004) not implemented | M1 review | M2 | `duplicate_vertices_tolerance` in diagnostics, with tolerance relative to bbox diagonal (default 1e-6) and spatial-hash or sort based (deterministic). Added to the M2 plan. Fixture with known near-duplicates. |
| MF-ISSUE-013 | S2 | Memory: 1.1 GB peak RSS at 4.8M triangles vs 400 MB budget | M1 report | **M2 (priority)** | A counting allocator plus a breakdown by category (mesh storage, topology, import scratch, history, GPU buffers) is reported by `--metrics` and the benchmark CI. M2's storage design is justified against these numbers before history and selections build on it. |
| MF-ISSUE-019 | S3 | Topology construction scratch is 468 MB at 4.8M triangles | ADR-0001 measurements | M4 | Topology build peak ≤ 150 MB at 5M (packed `u64` edge keys, no separate fan array), measured with `--metrics`. |
| MF-ISSUE-015 | S3 | No human-driven demonstration recording | M1 evidence | M1 sign-off | A screen recording of open → inspect → apply → undo → redo → export on a real machine, linked from `docs/milestones/m1.md`. |
| MF-ISSUE-016 | S3 | Font licence texts must ship with binaries | Font exception | First binary release | Installer or About dialog includes the texts in `licenses/third-party/`. See `THIRD_PARTY_NOTICES.md`. |
| MF-ISSUE-017 | S2 | Reference integrated-GPU hardware not available | Spec OD-2 | Before MF-RENDER-002 is declared met | Benchmarks run on a named 16 GB integrated-GPU laptop, with device info recorded. |
| MF-ISSUE-018 | S3 | Orientation repair not offered | Review of volume status | M3+ | `volume_status = inconsistent_orientation` gets a `mesh.orient_faces` operation (targetable). Until then, diagnostics distinguish the cause (done). |

## Closed

| ID | Title | Closed by |
|---|---|---|
| MF-ISSUE-014 | Cross-platform CI not yet run | CI run 42492436343 (PR #1): ubuntu, macOS and Windows each 84 verified passes, 0 skipped, 0 failed. Every GPU and GUI test ran on all three (lavapipe, Metal, WARP). |
| MF-ISSUE-010 | Non-atomic export could destroy the source file | `fix(io): atomic, verified export` · tests `failed_export_leaves_existing_file_intact`, `export_replaces_existing_file_atomically` |
| — | Binary STL export wrote `inf` for out-of-range coordinates (part of minor 3) | Same commit: `write_binary` rejects coordinates outside f32 |
| — | Volume hidden without saying why | `feat(geometry): volume_status` · test `volume_status_explains_missing_volume` |
| — | M1 review C1, I1, I2, I3 | See `docs/milestones/m1.md` |
