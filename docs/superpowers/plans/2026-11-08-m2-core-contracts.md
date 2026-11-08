# M2 Core Contracts Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prove the storage, identity and history model before anything is built on it. That means measured memory, structurally shared revisions with stable element identity, provenance, a revision DAG, selection translation, and verified replay.

**Architecture:** Measure first: a counting allocator with per-phase peaks, plus exact per-structure heap accounting. Then replace the flat mesh buffers with chunked, reference-counted storage. Faces and vertices get **stable slots**: deletion sets a tombstone, creation appends. Edits go through a `MeshEdit` builder that shares untouched chunks and records provenance as it goes. That makes provenance total by construction and keeps kept-face IDs stable across revisions, which is the foundation for localized editing and selections. History becomes a DAG of document states, each carrying a full command record. A JSONL command log replays headlessly with per-command `Exact`/`Equivalent`/`Mismatch` verification.

**Tech Stack:** As M1 (Rust 1.99.1 pinned), plus `rayon` (deterministic parallel sort) and a `mf-metrics` crate (the only crate allowed `unsafe`, for `GlobalAlloc`).

**Spec:** `docs/spec/phase0-spec.md` (v0.2). Milestone: `docs/plan/phase0-milestones.md` § M2. Issues: `docs/issues.md`.

## Global Constraints

- Licence MIT OR Apache-2.0. `cargo deny check licenses` passes. Rust 1.99.1 (`rust-toolchain.toml`); run cargo from the rustup toolchain, not Homebrew.
- `unsafe_code = "forbid"` everywhere except `mf-metrics`, `mf-render` and `mf-desktop` (existing).
- Working geometry `f64` positions and `u32` indices. Element counts must fit `u32` (MF-ISSUE-001).
- No silent fallbacks. Errors are structured. Skipped GPU tests go only through `mf_fixtures::skip`.
- All results are deterministic: no HashMap iteration order may reach output. Sorts use total-order keys.
- Every M1 test keeps passing. Where a test asserted the M1 storage layout (`triangles()` slices), it's ported to the new API with the same assertion intent.
- Budget gate (MF-GEO-002): retained working set (mesh + cached topology) for a 5M-triangle mesh is ≤ 400 MB.

## Review Focus

1. **Tombstoned slots leaking into consumers:** any reader that iterates raw slots (writers, topology, render, hash) instead of live elements would export deleted faces. Test: `export_skips_tombstoned_faces` in Task 3 (STL/OBJ round trip after removing faces).
2. **Undo across a structural edit then redo on another branch:** selection translation between sibling branches must go through the common ancestor, not assume a linear path. Test: `translate_across_branches` in Task 7.
3. **Replay when a source file moved or changed:** a source whose hash doesn't match must be a hard error naming the file, never a best-effort replay. Test: `replay_rejects_changed_source` in Task 8.
4. **Memory double-counting with shared chunks:** history accounting must count each shared chunk once. Test: `history_bytes_count_shared_chunks_once` in Task 3.
5. **Pinched boundary vertex with more than two loops meeting:** splitting must give each loop a deterministic order and stable IDs. Test: `three_loops_meeting_at_one_vertex` in Task 5.

---

## File structure

```text
crates/mf-metrics/src/{lib.rs, alloc.rs, phase.rs}          # new
crates/mf-geometry/src/{chunked.rs, ids.rs, edit.rs, compare.rs}  # new
crates/mf-geometry/src/{mesh.rs, topology.rs, diagnostics.rs}     # reworked
crates/mf-document/src/{lib.rs, geometry.rs, document.rs}   # new: Document/Object/ObjectId move here from mf-command
crates/mf-selection/src/{lib.rs, selection.rs, translate.rs}  # new (Provenance itself lives in mf-geometry)
crates/mf-command/src/{session.rs → history.rs, record.rs, log.rs, replay.rs}
crates/mf-cli/src/main.rs                                    # --metrics, replay, save-log
crates/mf-desktop/src/{app.rs, panels.rs}                    # save/load log, DAG view, UX issues
docs/adr/0001-stable-slot-chunked-storage.md                 # new
```

---

### Task 1: Memory instrumentation (MF-ISSUE-013), measured before any redesign

**Files:**
- Create `crates/mf-metrics/` (`[lints.rust] unsafe_code = "deny"`, with `#[allow(unsafe_code)]` only on the `GlobalAlloc` impl)
- Modify `crates/mf-geometry/src/{mesh.rs, topology.rs}` to add `heap_bytes()`
- Modify `crates/mf-cli/src/main.rs` and `scripts/evidence.sh`

**Interfaces (produces):**
- `mf_metrics::CountingAlloc` (wraps `System`), installed with `#[global_allocator]` in `mf-cli` only. `mf_metrics::{live_bytes() -> u64, peak_bytes() -> u64, reset_peak()}` read and reset atomics. When no counting allocator is installed they return 0 and `is_installed()` is false.
- `mf_metrics::Phase { name: String, wall_s: f64, peak_bytes: u64, retained_bytes: u64 }` and `mf_metrics::measure<R>(name, f: impl FnOnce() -> R) -> (R, Phase)`. `peak_bytes` is the peak above the live bytes at start. `retained_bytes` is the live bytes at end minus the live bytes at start (signed, saturating at 0).
- `Mesh::heap_bytes(&self) -> u64` (owned buffers) and `Topology::heap_bytes(&self) -> u64`.
- CLI: a global `--metrics <path.json>` flag writes `{ "phases": [Phase...], "structures": { "mesh": u64, "topology": u64 }, "peak_heap": u64 }`. Phases recorded: `import`, `topology`, `operation`, `export`.

- [ ] **Step 1: Tests**
  - `mf-metrics`: `measure_reports_peak_and_retained`. Allocate a 10 MB `Vec`, drop it, then allocate and keep 1 MB. Check `peak ≥ 10 MB` and `retained` ≈ 1 MB (±64 KB). Tests install the allocator in the test binary via `#[global_allocator]` in `tests/alloc.rs`.
  - `mf-geometry`: `heap_bytes_cube`, where `cube.heap_bytes() == 8*24 + 12*12` plus documented fixed overhead (assert a range).
  - `mf-cli`: `metrics_json_written`. `inspect --metrics m.json` gives JSON with phases `import` and `topology`, both `peak_bytes > 0`.
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5: Measure.** Run `scripts/evidence.sh` (extended to run 1M and 5M with `--metrics`). Record in `docs/adr/0001-stable-slot-chunked-storage.md` § Measurements: per-phase peaks, mesh bytes and topology bytes at 1M and 5M.
- [ ] **Step 6: Gate.** If mesh + topology at 5M is > 400 MB, stop. Record a ruling and redesign the topology representation before Task 3. Otherwise continue. (The expected result is ~140 MB retained and import scratch dominating the peak.)
- [ ] **Step 7:** Commit `feat(metrics): counting allocator, per-phase peaks, structure accounting [MF-ISSUE-013, MF-RT-005]`.

### Task 2: Import peak reduction

Do this only if Task 1 confirms import scratch dominates. Otherwise, rule and skip.

**Files:** `crates/mf-io/src/stl.rs`

**Approach:** Weld binary STL without materialising `Vec<[f64;3]>` corners. Sort corner indices by `[u32;3]` f32 bit keys read straight from the facet bytes, and drop the separate `leader`/`id_of_leader` arrays (assign IDs in one pass over the sorted order, then a second pass in corner order). Target: STL import peak ≤ file size + 2.5 × final mesh bytes.

- [ ] **Step 1: Test** `stl_import_peak_bounded` (mf-io test with the counting allocator). On a 200k-triangle generated STL, check `phase.peak_bytes ≤ file_len + 2.5 × mesh.heap_bytes()`. Existing STL tests must still pass, including `binary_with_solid_header` and `-0.0` welding.
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `perf(io): binary STL weld without f64 corner copies [MF-ISSUE-013]`.

### Task 3: Stable-slot chunked mesh storage and `MeshEdit` (MF-DOC-001/002, MF-SEL-001, ADR-0001)

**Files:**
- Create `mf-geometry/src/{chunked.rs, ids.rs, edit.rs}` and `docs/adr/0001-stable-slot-chunked-storage.md`
- Rework `mesh.rs`
- Port every consumer: `topology.rs`, `diagnostics.rs`, `mf-io` writers, `mf-operations`, `mf-render/gpu_mesh.rs`, `mf-fixtures`

**Interfaces (produces):**
- `FaceId(pub u32)` and `VertexId(pub u32)` (`Copy, Ord, Hash, Serialize, Deserialize`).
- `Chunked<T: Copy>`: `Arc<[Arc<[T]>]>` chunks of `CHUNK = 32_768` elements. API: `len()`, `get(i)`, `iter()`, `ptr_eq_chunk(&self, other, chunk_index) -> bool`, `unique_heap_bytes(seen: &mut HashSet<usize>) -> u64` (counts each chunk allocation once by address).
- `Mesh` fields: `positions: Chunked<[f64;3]>`, `faces: Chunked<[u32;3]>`, `face_live: Chunked<u64>` (bitset), `vertex_live: Chunked<u64>`.
  - `Mesh::new(positions, triangles)` keeps its M1 signature: all slots are live.
  - Rejects counts over `u32` (MF-ISSUE-001).
- Mesh accessors:
  - `face_slots() -> u32`, `face_count() -> usize` (live), `vertex_slots()`, `vertex_count()` (live)
  - `faces() -> impl Iterator<Item = (FaceId, [u32;3])>` (live, ascending)
  - `vertices() -> impl Iterator<Item = (VertexId, [f64;3])>`
  - `face(FaceId) -> Option<[u32;3]>`, `position(VertexId) -> Option<[f64;3]>`
  - `is_empty()`, `bounding_box()`, `content_hash()`, `topology()`, `heap_bytes()`
- `Mesh::compacted(&self) -> (Mesh, Compaction)`, where `Compaction { faces: Vec<Option<FaceId>>, vertices: Vec<Option<VertexId>> }` maps old slots to new ones. Writers export through it.
- `content_hash` v2: BLAKE3 over `b"mf-mesh-v2"`, then each live vertex `(id u32 LE, xyz f64 LE)`, then each live face `(id u32 LE, 3×u32 LE)`. Slot identity is part of content identity.
- `MeshEdit`, from `mesh.edit()`:
  - `remove_face(FaceId) -> Result<(), GeometryError>`
  - `remove_vertex(VertexId)` (it's an error if a live face uses it)
  - `add_vertex([f64;3]) -> VertexId`
  - `add_face([u32;3], sources: Sources) -> FaceId`
  - `move_vertex(VertexId, [f64;3])`
  - `finish(self) -> Result<(Mesh, Provenance), GeometryError>`
  - Only chunks that were written are copied (copy-on-write per chunk).
- `Provenance` (lives in `mf-geometry` so `mf-selection` can depend on it without a cycle):
  - `removed_faces: Vec<FaceId>`, `created_faces: Vec<(FaceId, Sources)>`
  - `moved_vertices: Vec<VertexId>`, `removed_vertices: Vec<VertexId>`, `created_vertices: Vec<VertexId>`
  - `Sources { faces: Vec<FaceId>, loops: Vec<u32> }`
  - Kept faces are implicit: live in both revisions, same ID, same indices.
- ADR-0001 records the decision, the alternatives considered (compaction per edit, which breaks chunk sharing and makes every edit a global renumbering; and per-revision ID maps), the consequences (all readers iterate live elements, explicit compaction on export, tombstone memory of 1 bit per slot) and the Task 1 measurements.

- [ ] **Step 1: Tests**
  - `chunked_cow_shares_untouched_chunks`: an edit touching one face of a 100k-face mesh shares all position chunks and all but one face chunk (`ptr_eq_chunk`).
  - `edit_remove_and_add_keeps_ids`: kept face IDs and indices are unchanged, new faces get IDs ≥ the old `face_slots`.
  - `provenance_is_total` (proptest, 256 cases): random edits, then every live output face is kept xor created, and every live input face is kept xor removed.
  - `export_skips_tombstoned_faces` (mf-io).
  - `compaction_maps_slots`.
  - `hash_includes_slot_identity`: same geometry with different slots gives a different hash.
  - `history_bytes_count_shared_chunks_once`.
  - `new_rejects_counts_over_u32`: a synthetic check through a `pub(crate)` validator that takes counts, so no 4 GB allocation is needed.
- [ ] **Step 2:** All M1 tests ported and green before the new tests go green.
- [ ] **Step 3–4:** FAIL → implement → PASS. Run the full workspace suite.
- [ ] **Step 5: Re-measure.** 1M and 5M `--metrics`. Retained mesh + topology at 5M must be ≤ 400 MB (MF-GEO-002 projection). Record it in ADR-0001.
- [ ] **Step 6:** Commit `feat(geometry): stable-slot chunked storage with copy-on-write edits and built-in provenance [MF-DOC-002, MF-SEL-001, MF-SEL-002]`.

### Task 4: Geometry kinds, identity versus equivalence (MF-DOC-003, MF-DOC-005)

**Files:**
- Create `crates/mf-document` (move `Document`, `Object`, `ObjectId` from `mf-command`) and `mf-geometry/src/compare.rs`

**Interfaces (produces):**
- `enum Geometry { Mesh(Mesh) }` with `kind() -> GeometryKind` (`Mesh`), `as_mesh() -> Option<&Mesh>`, `heap_bytes()` and `content_hash()`.
- `Object { id, name, geometry: Geometry, source }`. `Document` mutation methods are `pub(crate)`, exposed to `mf-command` through a sealed `DocumentMut` trait implemented in `mf-document` behind a `#[doc(hidden)] pub mod internal`, and documented as command-only (MF-CMD-001).
- `compare::equivalent(a: &Mesh, b: &Mesh, tolerance: f64) -> Equivalence { invariants_match: bool, vertex_hausdorff: f64, within: bool }`. Invariants are face count, component count, boundary-loop count, Euler characteristic and manifoldness. `vertex_hausdorff` is the symmetric maximum nearest-vertex distance via a uniform grid hash (deterministic).
- The dispatcher rejects ops whose spec `geometry_kinds` excludes the object's kind, with `CommandError::UnsupportedGeometry { op, kind }`.

- [ ] **Step 1: Tests**
  - `reordered_mesh_not_identical_but_equivalent` (spec MF-DOC-003 acceptance a)
  - `perturbed_equivalent_at_1e6_not_1e12` (acceptance b)
  - `dummy_kind_rejected_by_mesh_ops`: a `#[cfg(test)]` `Geometry::Dummy` variant behind a `test-kinds` feature. History handles it; `mesh.inspect` returns `UnsupportedGeometry`.
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(document): geometry kinds, identity vs equivalence [MF-DOC-003, MF-DOC-005]`.

### Task 5: Topology and diagnostics correctness debts

**Files:** `mf-geometry/src/{topology.rs, diagnostics.rs}`

**Interfaces (produces):**
- Boundary loops split at pinch vertices (MF-ISSUE-007). At a vertex with k outgoing and k incoming boundary half-edges, pair each incoming edge with the outgoing edge that comes next in the face fan around the vertex (walk the fan via the corner structure). If the fan order is ambiguous (non-manifold fan), fall back to pairing by smallest angle in the vertex's average tangent plane, with ties broken by vertex ID. `untraced_boundary_edges` then counts only edges at non-manifold-edge vertices.
- `Diagnostics` adds:
  - `duplicate_vertices_tolerance: usize` (tolerance `1e-6 × bbox_diag`, a grid hash with neighbour-cell checks, deterministic, MF-ISSUE-012)
  - `duplicate_faces: usize` (same sorted vertex triple, MF-ISSUE-008)
  - `non_finite: bool` (area, volume or perimeter overflow; `signed_volume` is `None` with `volume_status = NonFinite` instead of silently becoming `null`, MF-ISSUE-003)
- Topology uses `rayon::par_sort_unstable_by_key` on total-order keys (MF-CMD-005 groundwork).

- [ ] **Step 1: Tests**
  - `bowtie_holes_trace_as_two_loops` (6 edges → loops of 3 and 3, `untraced_boundary_edges == 0`)
  - `three_loops_meeting_at_one_vertex`
  - `near_duplicates_counted` (2 pairs within 1e-9, 1 pair at 1e-3: count 2)
  - `duplicate_face_counted`
  - `huge_coordinates_report_non_finite`
  - `topology_identical_at_1_and_8_threads` (`rayon::ThreadPoolBuilder` with 1 and 8 threads `install`, compare `Topology` with `PartialEq`)
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `fix(geometry): split pinched loops, tolerance and face duplicates, non-finite status [MF-ISSUE-007, -012, -008, -003]`.

### Task 6: Operations on `MeshEdit`; no-op without a revision

**Files:** `mf-operations/src/{lib.rs, remove_small.rs, inspect.rs}`

**Interfaces (changes):**
- `Operation::execute(&self, input: &Mesh, target: &Target, params: &Params) -> Result<OpOutput, OpError>`.
- `Target::{WholeObject, Faces(Vec<FaceId>), Components(Vec<u32>), Loops(Vec<u32>)}`. M2 ops accept `WholeObject` only; anything else gives `OpError::Failed { code: "unsupported_target" }`. The targeted variants exist now so M3 doesn't change the signature.
- `OpOutput { change: Option<(Mesh, Provenance)>, report }`. `change` is `None` when nothing changed: read-only, or `remove_small_components` removing nothing (MF-ISSUE-005).
- `remove_small_components` tombstones faces and the vertices used only by removed faces. No compaction.

- [ ] **Step 1: Tests** (port the M1 tests)
  - `removes_island` checks that `faces()` of the output equals the cube faces with **the same IDs**
  - `noop_returns_no_change`
  - `provenance_lists_removed_faces`
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(operations): edit-based ops with provenance; no-op creates no revision [MF-SEL-002, MF-ISSUE-005]`.

### Task 7: Revision DAG, command records, selection translation (MF-HIST-001, MF-CMD-002, MF-SEL-003/004)

**Files:**
- Create `mf-selection`
- Replace `mf-command/src/session.rs` with `history.rs` and `record.rs`

**Interfaces (produces):**
- `RevisionId(u64)` (monotonic) and `CommandId(u64)`.
- `CommandRecord` (serde) has:
  - `id`, `op_id`, `op_version`, `target: ObjectId`, `selection: Option<Selection>`, `params: Params`
  - `input_revision`, `input_hash`, `output_revision`, `output_hash` (object geometry hashes, hex)
  - `depends_on: Vec<CommandId>`, `affected_objects: Vec<ObjectId>`
  - `affected: AffectedSummary { removed: u64, created: u64, kept: u64 }`
  - `engine_version`, `platform` (`std::env::consts::{ARCH, OS}`), `duration_ms`, `status`
- `Session` keeps the M1 API (`import`, `prepare`/`run`/`commit`, `execute`, `undo`, `redo`, `can_*`, `history_labels`, `history_cursor`) over a DAG.
  - `redo` goes to the most recently created child.
  - A new command after undo adds a sibling; the old branch is retained.
  - New: `revisions() -> &[RevisionNode]`, `node(RevisionId)`, `current() -> RevisionId`, `checkout(RevisionId) -> bool` (jump to any node), `records_to_current() -> Vec<CommandRecord>`.
  - `RevisionNode { id, parent: Option<RevisionId>, record: Option<CommandRecord>, import: Option<ImportRecord>, children: Vec<RevisionId> }`.
- `Selection { object: ObjectId, revision: RevisionId, kind: SelectionKind, ids: Vec<u32> }` with `SelectionKind::{Object, Faces, Component, BoundaryLoop}` (MF-SEL-004).
- `Session::translate(&self, sel: &Selection, to: RevisionId) -> Translation`, returning:
  - `Mapped(Selection)`
  - `Partial { mapped, lost: Vec<u32> }`
  - `Ambiguous { candidates }`
  - `Failed(TranslateError::{NoPath, ObjectMissing, LoopClosed { faces: Vec<FaceId> }, KindNotTranslatable})`

  The path goes from `sel.revision` up to the lowest common ancestor and down to `to`. Going up inverts provenance (created faces are lost). Going down applies it (removed faces are lost). Face selections map by stable ID. Component and boundary-loop selections are converted to the face sets or loop vertex IDs of that revision and re-resolved at the destination. A loop that no longer exists is `Failed(LoopClosed)` with the faces whose sources include it, or `Partial` when it was split.

- [ ] **Step 1: Tests**
  - `dag_retains_abandoned_branch` (spec MF-HIST-001: A, B, undo, C means B and C are children of A)
  - `undo_redo_hashes` (M1 port)
  - `record_roundtrips_json`
  - `face_selection_maps_across_removal` (`Partial` with the removed island faces listed)
  - `translate_across_branches`
  - `stale_commit_rejected` (port)
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(command): revision DAG with retained branches, full command records, selection translation [MF-HIST-001, MF-CMD-002, MF-SEL-003]`.

### Task 8: Command log save/load and verified replay (MF-CMD-004, OD-3)

**Files:** `mf-command/src/{log.rs, replay.rs}` and `mf-cli/src/main.rs`

**Interfaces (produces):**
- On-disk log: `<name>.mfx.log/manifest.json` with `{ format: "mfx-log/1", engine_version, sources: [{ object: ObjectId, path (relative to the log dir when possible), content_hash, format }] }`, plus `commands.jsonl` (one `CommandRecord` per line, root to current).
- `Session::save_log(&self, dir: &Path) -> Result<(), LogError>`.
- `replay(dir: &Path) -> Result<ReplayReport, LogError>`:
  - Re-imports sources and checks their hashes. A mismatch is `LogError::SourceChanged { path, expected, actual }`.
  - Re-executes each record and classifies it: `Exact` (hash equal), `Equivalent { hausdorff }` (equivalence at tolerance `1e-9 × bbox_diag`) or `Mismatch`.
  - `ReplayReport { results: Vec<(CommandId, Verdict)>, session: Session }`.
- CLI:
  - `meshfixer run <in> --op <id> [--param k=v]… --save-log <dir> -o <out>` runs a sequence of ops; `--op` can repeat, and `--param` applies to the preceding `--op`.
  - `meshfixer replay <dir> [-o <out>]` prints one verdict per command. Exit 0 if all are `Exact`/`Equivalent`, 1 otherwise.

- [ ] **Step 1: Tests**
  - `save_load_replay_exact`
  - `tampered_log_mismatch`: edit `params` in a line, so replay gives `Mismatch`
  - `replay_rejects_changed_source`
  - `replay_cli_exit_codes`
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(command): command log and verified replay [MF-CMD-004]`.

### Task 9: Determinism harness (MF-CMD-005 strict level)

**Files:** `tests/determinism` (workspace-level test in `mf-command/tests/determinism.rs`)

- [ ] **Step 1: Test** `ops_identical_at_1_and_n_threads`: for every registry op on the damaged fixture, run inside 1-thread and 8-thread rayon pools and compare output `content_hash`.
- [ ] **Step 2:** CI cross-OS equivalence job. The Linux job writes `replay-fixture.mfx.log` plus output hashes as an artifact. The macOS and Windows jobs download it, replay it, and require every verdict to be `Exact` or `Equivalent` (only the job summary distinguishes the two).
- [ ] **Step 3:** Commit `test: strict determinism across thread counts; cross-OS replay equivalence in CI [MF-CMD-005]`.

### Task 10: Desktop integration and M2-tagged UX debts

**Files:** `mf-desktop/src/{app.rs, panels.rs, viewport.rs}` and `crates/mf-desktop/tests/workflow.rs`

**Behaviour:**
- File → Save Command Log… / Replay Command Log… (the replay report goes in a window).
- A History panel shows the DAG as an indented tree. Retained branches are greyed out, and clicking a node jumps to it (`Session::checkout`, Task 7).
- MF-ISSUE-006: show "Empty mesh (0 triangles)" for an empty object.
- MF-ISSUE-009:
  - Undo/redo shortcuts are ignored while a text field has focus (`ctx.memory(|m| m.focused().is_some())`).
  - The Edit menu is disabled while busy.
  - `Viewport` drops `uploaded` when there's no active object.
- MF-ISSUE-004: show all import warnings (a list in the status panel, not just the last one). Replace `unwrap_or_default()` in `viewport.rs` and `unwrap_or(0)` in `topology.rs` with explicit invariants. The CLI `to_string_pretty` error propagates. Add a CI grep step that fails on new `unwrap_or_default()` outside `tests/` (allowlist file `scripts/allow-unwrap-or-default.txt`).

- [ ] **Step 1: Test.** Extend `workflow.rs`: apply, undo, apply with different params (a branch), check the DAG has 2 children, save the log to a temp dir, then `replay` gives all `Exact`.
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(desktop): command log save/replay, DAG history view, UX fixes [MF-HIST-001, MF-ISSUE-004/006/009]`.

### Task 11: M2 close-out

- [ ] Full suite on 1.99.1, clippy, deny. Push branch `m2-core-contracts` and open a PR. CI must be green with the skip summary at 0 on Linux.
- [ ] Evidence: `--metrics` at 1M and 5M (before and after Task 2/3), ADR-0001, DAG screenshot, and replay report.
- [ ] Independent whole-branch review (most capable model). Its brief includes "challenge shortcuts that pass acceptance tests without advancing localized editing". Fix Critical/Important findings with tests that fail first. Minors go into `docs/issues.md`.
- [ ] `docs/milestones/m2.md` with gates, measurements and learnings. Close the issues fixed in this milestone in `docs/issues.md`.
