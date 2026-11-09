# ADR-0001: Stable-slot chunked mesh storage

Status: Accepted (M2 Task 3) · 2026-11-08

## Context

MeshFixer must support localized, surgical editing with undo, editable history and selections that survive edits (spec MF-DOC-002, MF-SEL-001…003, MF-OP-003). M1 stored a mesh as two flat `Vec`s, so every revision was a full copy, and any deletion compacted and renumbered every element after it. Under that model a selection made before an edit means nothing afterwards without a full remap.

## Decision

1. **Chunked copy-on-write storage.** Positions, faces and liveness bitsets are stored in chunks of 32,768 elements behind `Arc`. An edit copies only the chunks it writes. A revision shares every other chunk with its parent.
2. **Stable slots with tombstones.** Removing a face or vertex clears its liveness bit. Its slot is never reused. Creating one appends a new slot. A kept element therefore has **the same ID in every revision**, which is what makes selection translation (MF-SEL-003) cheap and exact.
3. **One edit path that records provenance.** All changes go through `MeshEdit`, which records removed, created (with sources), moved and kept elements as it goes. Provenance is total by construction (`provenance_is_total`).
4. **Explicit compaction.** Export iterates only live elements (renumbering densely), and `Mesh::compacted()` returns a tombstone-free mesh plus slot maps when one is needed.
5. **Content identity includes slot IDs** (`mf-mesh-v2` hash). Two meshes with the same live geometry but different slot layouts are different revisions. Use the equivalence comparator (MF-DOC-003) to compare geometry.

## Alternatives considered

- **Compact on every edit and carry a per-edit renumbering map.** Rejected. Every deletion becomes a global renumbering, which breaks chunk sharing (all later chunks change) and makes every selection go through an O(n) map.
- **Per-revision ID maps over flat arrays.** Rejected. It costs O(n) memory per revision, which is exactly what MF-DOC-002 forbids.
- **A half-edge structure as the primary representation.** Deferred. It's heavier per element and not needed for the M2 contracts. Topology stays derived and cached (MF-GEO-003).

## Consequences

- Every reader must iterate live elements (`faces()`, `vertices()`) or index by slot (`corner_position`). The `export_skips_tombstoned_faces` test guards the writers.
- Tombstones cost 1 bit per slot. Long editing sessions accumulate dead slots. A future `compact` command (which emits a provenance map like any other edit) can reclaim them; tracked as MF-ISSUE-020.
- Topology arrays are sized by slots, not live elements, so dead slots cost a few bytes each in transient topology scratch.
- `Mesh::new` still copies its input `Vec`s into chunks, so construction briefly holds about 2× the mesh. That accounts for most of the import peak rising from 374 to 404 MB. It's acceptable, and readers can build chunked storage directly later if import peak matters.
- **GPU geometry follows the storage chunks (M4 Task 6, MF-RENDER-003).** The renderer keeps one buffer set per face chunk. A new revision reuses a GPU chunk of the revision it replaces on screen when `Mesh::face_chunk_shared` holds for it (same face storage *and* the same liveness bits) and `Mesh::vertex_chunk_kept` holds for every vertex chunk its faces reference (shared, or only appended to). The test is made against the replaced revision's own storage, never by assuming history is linear, so undo, redo and checkout to another branch can never leave a stale chunk. Buffers are immutable once uploaded and shared through `Arc`, so cached revisions stay valid. The local origin is carried over unless the mesh reaches more than 10³ diagonals from it, and in that case everything is rebuilt. Measured: a one-hole fill on 1M triangles uploads 1.1 MB instead of 72 MB, and the app's fill-all-holes on 5M uploads 10 MB of 355 MB.

## Measurements (M2 Task 1, before any redesign)

Release build, Apple M2 Max, `meshfixer … --metrics`. Heap bytes are from the counting allocator. "Peak" is the peak above the level at the start of the phase.

| Mesh | Phase | Wall | Peak | Retained |
|---|---|---|---|---|
| 0.96M tri (46 MB STL) | import | 0.36 s | 202 MB | 22 MB |
| | topology | 0.18 s | 91 MB | 4 MB |
| | remove_small (operation) | 0.20 s | 91 MB | 28 MB |
| | export (verified) | 0.35 s | 202 MB | 0 |
| 4.8M tri (230 MB STL) | import | 1.93 s | **1017 MB** | 110 MB |
| | topology | 0.98 s | 468 MB | 32 MB |
| | remove_small (operation) | 1.21 s | 468 MB | 224 MB |
| | export (verified) | 2.23 s | **1017 MB** | 0 |

The process-wide heap peak for the 4.8M `remove-small-components` run was 1383 MB.

**Retained working set at 4.8M: mesh 110 MB + topology 32 MB = 142 MB.** That's within the 400 MB budget (MF-GEO-002), so the gate passes.

The 1.1 GB figure from M1 came almost entirely from transient scratch:
1. **Import:** the 230 MB file buffer, plus f32 corners, f64 corners and three index arrays for welding.
2. **Export verification:** re-parsing and re-welding the encoded file, which costs the same as an import.
3. **Topology construction:** a 16-byte half-edge per corner, plus a fan array and corner union-find (468 MB at 4.8M).
4. **remove_small output:** `Vec` growth overshoot (192 MB instead of ~110 MB).

Items 1 and 2 are addressed in Task 2, and item 4 by `MeshEdit` in Task 3. Item 3 is tracked: topology scratch can drop to roughly a third by keying half-edges as `u64` and using in-place union-find (MF-ISSUE-019).

### After Task 2 (import weld rewrite, structural export verification)

| 4.8M tri | Before | After |
|---|---|---|
| import peak | 1017 MB | **374 MB** (file 230 MB + ~144 MB scratch) |
| import wall time | 1.93 s | 1.77 s (parallel sort, deterministic: `stl_weld_identical_at_1_and_8_threads`) |
| export peak | 1017 MB | **230 MB** (the encoded bytes; the binary STL check is structural) |
| process peak, remove-small run | 1383 MB | 802 MB |

The remaining large transient is topology construction (468 MB), tracked as MF-ISSUE-019.

### After Task 3 (stable-slot chunked storage)

| | 0.96M tri | 4.8M tri |
|---|---|---|
| mesh (retained) | 22 MB | 111 MB |
| topology (retained) | 4 MB | 32 MB |
| **working set (gate ≤ 400 MB at 5M)** | 26 MB | **143 MB ✓** |
| remove_small: retained by new revision | 4 MB | 32 MB (topology only; storage shared). Was 224 MB |
| process peak, remove-small run | 118 MB | 612 MB. Was 1383 MB |
