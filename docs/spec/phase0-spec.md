# MeshFixer Phase 0 Engineering Spec (v0.5)

v0.5 (2026-11-09, owner request): adds modelling tools and plugin/scripting requirements (§11b): booleans and intersection, splitting with alignment pegs and joints, kitbashing parts onto models, parametric connectors (hinges, clasps, joints), an object transform gizmo, plugins and scripting. Phase 0 must not block them; the M4 and M5 reviews check this.

v0.4 (2026-11-08, M4 Checkpoint A): fairing is required (MF-OP-002, fill styles). Many-hole handling is MF-RENDER-006. Sculpting and carving are captured as Phase 1 requirements (§11a), which Phase 0 must not block. The owner also approved (2026-11-09) the §2 dependency change from M4 Task 4: `mf-geometry` may depend on `mf-runtime`, so topology can be built cancellably.

v0.3 (2026-11-08, M3 review): MF-OP-002 corrected to Liepa's ordering (worst dihedral, then area); it now states the whole-object reporting behaviour, the 1000-edge cap and the untriangulable case.

Status: Approved for implementation · 2026-11-08
Licence: MIT OR Apache-2.0 · Dev machine: Apple M2 Max, 32 GB, macOS

## Purpose

This spec constrains and falsifies the Phase 0 spike. It narrows **what we build first**, not **what the architecture can become**. Wherever a decision is deliberately temporary, the spec says how it evolves (marked **Evolves:**). Requirements are numbered. Each one has a pass/fail acceptance check.

Phase 0 answers six questions:

1. Does document → command → history work end to end (execute, undo, redo, serialize, replay)?
2. Are operations deterministic enough to test with declarative postconditions?
3. Can one operation definition drive GUI, CLI, MCP schema, validation, serialization and tests?
4. Does the app stay interactive on large, *damaged* geometry while work runs in the background?
5. Does element identity survive topology-changing operations through an explicit, tested contract?
6. Can a user operate **surgically**: pick one defect, repair only that, and leave everything else untouched?

## 1. Settled decisions

| Decision | Phase 0 choice | Evolves |
|---|---|---|
| Language | Rust (stable) | — |
| Desktop UI | egui (eframe), behind `mf-desktop` only | Slint/other UI can replace `mf-desktop` without touching the core |
| Rendering | wgpu in `mf-render`, no egui dependency | Alternate frontends reuse `mf-render` |
| Working geometry | Indexed triangle mesh, `f64` positions | Additional geometry kinds (§3, MF-DOC-005) |
| Render geometry | `f32`, relative to a per-object local origin | LOD/chunk streaming in Phase 1 |
| Formats | STL, OBJ, 3MF read/write | PLY, glTF, STEP tessellation later; B-rep is a separate program |
| History | Revision DAG, linear navigation only | Branch navigation, parameter editing, downstream recompute |
| Operation definitions | Versioned YAML specs compiled into the binary | Plugin-provided specs |
| Automation | CLI. MCP tool schemas generated (server optional) | Full MCP server in Phase 1 |
| Scripting, plugins | None | Phase 2 |
| Platforms | Windows, macOS, Linux in CI from day one | — |

## 2. Workspace layout

```text
crates/
  mf-geometry    # mesh, topology queries, diagnostics, canonical hashing
  mf-io          # STL / OBJ / 3MF
  mf-document    # objects, geometry kinds, revisions, revision DAG
  mf-selection   # selections, provenance maps, translation
  mf-spec        # operation spec loading, param schema, validation, schema export
  mf-command     # command records, execution, undo/redo, log, replay
  mf-operations  # inspect, remove_small_components, fill_holes, (simplify)
  mf-runtime     # jobs, cancellation, progress, thread budget, metrics
  mf-render      # GPU buffers, camera, picking, overlays, offscreen
  mf-cli
  mf-desktop
specs/operations/   tests/geometry/   fixtures/
```

Dependency direction: `mf-geometry` depends on nothing in the workspace except `mf-runtime` (cancellation and progress, itself dependency-light: `rayon` and `thiserror`), so topology and diagnostics can be cancelled (MF-RT-002). Frontends (`mf-cli`, `mf-desktop`) are leaves. Only `mf-desktop` depends on egui. `mf-render` depends on geometry and selection types, never on documents or commands.

---

## 3. Contract A: Geometry representations and document state

**MF-GEO-001: Three representations.** There are three distinct roles: *source* (what was imported), *working* (CPU operations) and *render* (GPU). They're conceptual boundaries and may share storage.
- **Source:** the original file is referenced by content hash and path. Its declared precision, units and metadata are recorded on the object.
- **Working:** `f64` positions, `u32` triangle indices, held in reference-counted chunks.
- **Render:** `f32` positions relative to the object's local origin (the bounding box centre), so precision holds far from the world origin.

*Accept:* Importing an OBJ with 15-significant-digit coordinates and exporting OBJ preserves them exactly (relative error < 1e-12). Render buffers are never read back as geometry.

**MF-GEO-002: Memory budget.** For a 5M-triangle mesh, working geometry plus all cached derived topology is ≤ 400 MB. This is measured as allocated heap bytes using a counting allocator in the benchmark binary. GPU buffers and render copies are excluded and reported separately.
*Accept:* `bench-memory` reports both figures. The budget is a gate; a miss fails the milestone, and the fix is not to change the number.

**MF-GEO-003: Lazy derived topology.** Adjacency, boundary loops, components and non-manifold edges are computed on demand and cached per revision. They're never authoritative.
*Accept:* No topology is computed at load. A repeated query returns the cached result (pointer-equal `Arc`).

**MF-GEO-004: Diagnostics.** `mesh.inspect` reports: triangle and vertex counts, connected components, boundary edges, boundary loops (each with an ID, edge count, perimeter and centroid), non-manifold edges and vertices, degenerate triangles, exact and tolerance duplicate vertices, inconsistent-orientation edges, bounding box, area, signed volume (when closed), and is-closed/is-manifold.
*Accept:* Every metric has a fixture with a known expected value.

**MF-DOC-001: Logically immutable revisions.** Every change produces a new object revision. Existing revisions are never mutated.
*Accept:* After any operation, the input revision's content hash is unchanged.

**MF-DOC-002: Structural sharing.** Revisions share unchanged chunks (copy-on-write). Storing full geometry for every revision isn't allowed.
*Accept:* Ten successive targeted `fill_holes` revisions on a 5M-triangle mesh use < 1.5× the memory of a single revision.
*Evolves:* deltas, compressed checkpoints and eviction plus regeneration by replay.

**MF-DOC-003: Identity vs equivalence.** Two separate notions:
- **Content hash:** BLAKE3 over a canonical byte serialization (positions as IEEE-754 little-endian, indices, attributes, in stored order). Equal hashes mean byte identity. Different vertex or face order means different hashes, by design.
- **Geometric equivalence:** a comparator reporting topology invariants (components, boundary loops, Euler characteristic, manifoldness) plus a symmetric Hausdorff distance bound. Used where exact identity can't be expected.

*Accept:* Tests show (a) the same mesh reordered gives a different hash but is equivalent, and (b) a mesh perturbed by 1e-9 is equivalent at tolerance 1e-6 and not at 1e-12.

**MF-DOC-004: Multi-object documents.** A document holds objects with stable `ObjectId`s (never reused within a document), names, transforms and explicit units (default mm).
*Accept:* A three-object 3MF round-trips names, transforms and units.

**MF-DOC-005: Geometry is not synonymous with triangles.** Object geometry is a tagged kind (`Geometry::Mesh` is the only Phase 0 variant). Commands and operation specs declare which geometry kinds they accept, and the dispatcher rejects others with a structured error. No document, history or command API takes a mesh type directly.
*Accept:* A test adds a dummy geometry kind under `cfg(test)` and confirms the history and command layers handle it unchanged, while mesh operations reject it cleanly.
*Evolves:* `PointCloud`, `Brep`, `Volume`.

## 4. Contract B: Commands and history

**MF-CMD-001: Single mutation path.** All document changes go through `mf-command`. Frontends can't mutate documents directly.
*Accept:* Enforced by visibility. Document mutation methods are `pub(crate)` or sealed behind `mf-command`.

**MF-CMD-002: Command record.** Each record has:
- a `CommandId` that is stable and unique within the document,
- operation ID and version,
- the target (`ObjectId` plus an optional selection),
- validated parameters,
- input revision IDs and hashes, output revision IDs and hashes,
- `depends_on` (the command IDs that produced each input revision),
- affected objects, plus an affected region summary (removed, created and kept face counts),
- engine version, platform triple, duration and status.

*Accept:* Records round-trip through JSON losslessly.

**MF-HIST-001: Revision DAG.** History is a DAG of document states. Each state has a parent and the command that produced it. Undo/redo move a cursor along one path. Running a command after undo creates a new child, and the abandoned branch is **retained** in the model, though not navigable in the Phase 0 UI.
*Accept:* Execute A, B, undo, C. The DAG contains both B and C as children of A, and the cursor is at C.
*Evolves:* branch navigation UI and a retention/eviction policy.

**MF-HIST-002: Future-editing invariants.** To enable later parameter editing and downstream recompute without changing the record format, every record must be sufficient to re-execute its command against a *different* input revision. That means the parameters plus a selection expressed in a remappable form (MF-SEL-003), never raw positions.
*Accept:* A test re-executes a recorded `fill_holes` on a regenerated upstream revision, with remapping, and gets a valid result.

**MF-CMD-003: Undo/redo.** Moves the cursor without recomputation.
*Accept:* A, B, C; undo ×2; redo ×1 → the current hash equals the hash after B.

**MF-CMD-004: Replay with explicit verification.** A command log plus its source files (checked by hash) replays headlessly. Replay reports one result per command:
- `Exact`: the hash matches,
- `Equivalent`: the hash differs, but geometry is within tolerance and the invariants match (expected across platforms),
- `Mismatch`: a hard error.

A source-hash mismatch is always a hard error.
*Accept:* A same-platform replay gives all-`Exact`. A tampered log produces `Mismatch`.

**MF-CMD-005: Two levels of determinism.**
- **Strict:** with the same engine version, platform, inputs and parameters, output is byte-identical *regardless of thread count*. This is required for every Phase 0 operation.
- **Cross-platform:** output is geometrically equivalent (MF-DOC-003) across Windows, macOS and Linux. Byte identity is not claimed.

*Accept:* CI runs the geometry tests at 1 and N threads on each OS and compares hashes. A cross-OS job compares results by equivalence.

**MF-CMD-006: Transactional failure.** An operation computes into new storage and commits only at the end. Failure or cancellation produces no revision, leaves the document unchanged, and returns a structured error (code, message, element refs). No partial results, no silent fallbacks.
*Accept:* Fault-injection and cancel tests confirm the document hash is unchanged.

## 5. Contract C: Selection, identity and provenance

**MF-SEL-001: Identity model.**
- `ObjectId` is stable for the document's lifetime.
- `FaceId`, `VertexId` and `BoundaryLoopId` are scoped to a revision: indices that are meaningful only together with that revision's ID.
- An operation must keep faces outside its target untouched *and* preserve their relative order. Compaction is allowed, provided the provenance map records it.

*Accept:* Covered by MF-SEL-002 and MF-OP-003.

**MF-SEL-002: Provenance map.** Every topology-changing operation returns a face provenance map. Each output face is either `Kept(input face)` or `Created { sources }`, where the sources reference input faces or boundary loops. Every input face not kept is listed as `Removed`.
*Accept:* For each Phase 0 operation, a property test checks totality: every output face is accounted for exactly once, and every input face is either kept exactly once or removed.

**MF-SEL-003: Selection translation.** `translate(selection, from_rev, to_rev)` composes provenance maps along the DAG path and returns one of:
- `Mapped(selection)`: everything mapped,
- `Partial { mapped, lost }`,
- `Ambiguous { candidates }`,
- `Failed(reason)`, e.g. no path, or an incompatible geometry kind.

Selections bound to one revision are never applied to another without translation.
*Accept:*
- A face selection made before `fill_holes` maps fully afterwards (`Mapped`).
- A selection that includes a removed island returns `Partial`, with the lost faces listed.
- Translating a boundary-loop selection across the `fill_holes` that closed that loop returns `Failed(LoopClosed)` and names the faces that closed it.

*Evolves:* geometric reprojection for remeshing (Phase 1+). The API's result types already cover it.

**MF-SEL-004: Selection kinds in Phase 0.** Object, face set, connected component, and boundary loop. A selection is stored as data (element IDs + revision ID), never as GUI state.
*Accept:* Selections serialize inside command records.

## 6. Contract D: Operation specification and targeted operations

**MF-SPEC-001: Declarative definitions.** `specs/operations/<id>.yaml` contains: `id`, `version`, `summary`, `geometry_kinds`, `target` (accepted selection kinds, plus whether *whole object* is allowed), `parameters` (type, default, range, unit, description), `preconditions`, `postconditions`, `effects`, and `runtime` (`cancellable`, `progress`).
*Accept:* The specs validate against `specs/operation.schema.json` in a unit test.

**MF-SPEC-002: Parity.** Each spec has exactly one implementation with a matching version, and each implementation has a spec.
*Accept:* A unit test fails on any mismatch.

**MF-SPEC-003: One definition, every interface.** From the spec alone the system derives: GUI parameter controls, CLI arguments, MCP tool JSON Schema (`meshfixer schema --mcp`), validation and log serialization. No frontend may hand-write operation parameters.
*Accept:* Adding a parameter to a spec (and using it in the implementation) makes it appear in the GUI panel, `--help` and the MCP schema with no frontend code change. A golden test compares the CLI and MCP schemas derived from the same spec.

**MF-SPEC-004: Declarative geometry tests.** `tests/geometry/*.yaml` cases take the form given fixture (+ optional target selector) → when operation(params) → expect metrics, an `untouched_outside_target: true` check, and a provenance-totality check.
*Accept:* They run under `cargo test` with per-case reporting.

**MF-OP-001: Phase 0 operations, in order.**
1. `mesh.inspect`
2. `mesh.remove_small_components` (target: object, or chosen components; threshold by triangle count and/or area)
3. `mesh.fill_holes` (target: object, meaning all loops up to a size limit, or chosen boundary loops)
4. `mesh.simplify` (stretch goal)

**MF-OP-002: Hole-fill quality.** `fill_holes` uses a minimum-weight triangulation of the boundary loop (dynamic programming, minimizing worst dihedral then area, after Liepa 2003). It never uses a chord that is already a mesh edge, or a zero-area triangle. It then **refines** the patch so its interior edge lengths match the mean boundary edge length. Fan triangulation is not acceptable as the Phase 0 algorithm. A selected loop that can't be filled (above `max_loop_edges`, default 500 and at most 1000; self-touching; or untriangulable, such as a collinear slit) fails with a structured error naming the loop. A whole-object run fills the rest and lists each such loop by ID and reason in its report. No loop is ever silently skipped or fan-filled. **Fill styles:**
- `flat`: triangulate and refine only.
- `smooth`, the default: bi-Laplacian fairing (Δ²x = 0) of the patch interior, with the boundary and 1-ring fixed, so the patch meets the surface with matching slope (C1).
- `curvature`: tri-Laplacian (Δ³x = 0), 2 rings fixed (C2).

`smooth` and `curvature` always refine (fairing moves the added vertices; without them the patch would be flat). Fairing moves only the patch's interior vertices, is deterministic, and fails by name if it can't converge. It never silently falls back to flat. Records of `fill_holes` version 1 mean `flat`. Known limit (MF-ISSUE-055, open): `curvature` is accurate to 1% of the surface only on holes of up to 60 edges; a curvature fill of a larger hole keeps its style, marks the loop `curvature_less_accurate` in the report and adds a plain-language `warnings` entry recommending `smooth`.
*Accept:*
- On a 44-edge hole in a unit-radius sphere (the cap within 30° of a pole removed), every patch vertex is within 2% of the radius with `smooth`, and within 1% with `curvature`. The limit depends on the cap: the continuous Δ² solution overshoots by ≈ 1.04% of the radius at 30° and ≈ 2.0% at 35°, so a change in refinement density can move the measurement by a few tenths of a percent.
- On procedural holes, the patch's mean edge length is within ±25% of the boundary's mean, and no patch triangle has a minimum angle below 5°.
- On the real scan fixture, every targeted loop closes, the result is manifold around each patch, and an offscreen render is captured for review.

**MF-OP-003: Surgical guarantee.** When an operation runs on a target, geometry outside it is unchanged. Every kept face's vertex positions are bitwise-equal to the input, and kept faces keep their relative order.
*Accept:* `untouched_outside_target` is asserted in every targeted declarative test, including on the real scan.

## 7. Contract E: Runtime and resources

**MF-RT-001: No geometry on the UI thread.** No operation, import, export or O(n) topology work runs on the UI thread.
*Accept:* With instrumentation on, no UI frame exceeds 50 ms while `fill_holes` runs on a 5M-triangle mesh.

**MF-RT-002: Cooperative cancellation.** Long work checks a cancellation token at least every 100 ms of work. Combined with MF-CMD-006, cancellation leaves no partial state.
*Accept:* A measured cancel-to-stop time of ≤ 250 ms on a 5M-triangle import and a large `fill_holes`.

**MF-RT-003: Progress.** Progress is monotonic in 0–1 with a stage label, shown in the GUI and by CLI `--progress`.

**MF-RT-004: Thread budget.** A dedicated pool leaves at least one core free by default. The size is configurable and shown in diagnostics.

**MF-RT-005: Telemetry.** Operation durations, peak heap, GPU buffer bytes and frame-time p50/p95/p99. Shown in the GUI debug panel and written as JSON by CLI `--metrics`.

## 8. Contract F: Rendering and interaction

**MF-RENDER-001: Renderer independence.** `mf-render` has no egui dependency and renders offscreen.
*Accept:* `meshfixer render in.stl -o out.png` works headlessly.

**MF-RENDER-002: Interactive targets (validation gates).**

| Mesh size | Target |
|---|---|
| 1M triangles | ≥ 60 fps |
| 5M triangles | ≥ 30 fps |
| 10M triangles | navigable, ≥ 15 fps |

Measured by an automated orbit benchmark on the *damaged* fixtures. Every benchmark result records the device's CPU, GPU, RAM, OS and display resolution.
*Accept:* Phase 0 results on the M2 Max are **preliminary**. Integrated-GPU targets are not considered proven until they run on real lower-spec hardware (tracked as open item OD-2). Throttling the M2 Max is not accepted as proof.

**MF-RENDER-003: Incremental updates.** A selection change touches only the per-face state buffer. A geometry change re-uploads only changed chunks.
*Accept:* Upload byte counters confirm both.

**MF-RENDER-004: Picking.** Face picking takes < 16 ms at 5M triangles. Boundary-loop picking (clicking near a highlighted boundary) resolves to a `BoundaryLoopId`.

**MF-RENDER-006: Many defects.** Models with hundreds or thousands of holes stay workable:
- holes are grouped by size, with a count and an action per group;
- batch fill up to a size, with a preview count, as one undoable command;
- marking intentional openings (persistent, excluded from counts and bulk fills);
- next/previous navigation;
- multi-select and box select in the viewport;
- an overview that makes tiny holes visible when zoomed out.

*Accept:* the GUI tests on a 300-hole fixture, and the M4 demo.

**MF-RENDER-005: Diagnostic overlays.** Boundary loops, non-manifold edges and small components are highlighted, each one clickable to select it.

## 9. I/O, platform and legal

**MF-IO-001: Formats.** STL (binary and ASCII), OBJ (`v`/`f`; other lines skipped and counted as warnings), and 3MF core (multiple objects, transforms, units). Readers stream and report progress.
*Accept:* Round trips per format. Exported 3MF opens in at least one mainstream slicer (manual check, recorded in the milestone report).

**MF-IO-002: Hostile input.** Malformed or truncated input gives structured errors, never panics, unbounded allocations or hangs.
*Accept:* A `cargo-fuzz` target per reader. CI runs a time-boxed smoke test.

**MF-PLAT-001: Cross-platform CI.** Build and test on Windows, macOS and Linux for every push. Platform-specific code lives only in `mf-desktop` and marked adapter modules.

**MF-ARCH-LEGAL-001: Licence discipline.** No mandatory dependency may impose copyleft obligations incompatible with MIT OR Apache-2.0. Optional components with stronger licences go behind replaceable boundaries and need review before adoption.
*Accept:* `cargo deny check licenses` in CI with an allowlist.

## 10. Fixtures

- **Procedural** (`mf-fixtures` crate): seeded, deterministic primitives with controlled damage (holes, islands, duplicates, degenerates, flipped faces, non-manifold fins), scalable to 1M, 5M and 10M triangles, generated at test or benchmark time.
- **Hand-made small fixtures** are checked in.
- **Real damaged scans are first-class benchmarks**, not extras. At least one permissively licensed photogrammetry or phone scan with real holes and islands, recorded in `fixtures/SOURCES.md` with its licence. Large files are fetched by script with their hash verified, not committed.

## 11. Non-goals for Phase 0

STEP/B-rep, fillets/chamfers, remeshing, smoothing, booleans (see §11b), point clouds, textures, surface text, pattern recognition, semantic tags, branch navigation UI, parameter editing UI, plugins and scripting (see §11b), LLM planning, installers and signing, and visual polish. None may be blocked by Phase 0 design. Where a Phase 0 choice touches one, the relevant **Evolves:** note says how.

## 11a. Phase 1 requirements captured now: sculpting and carving

These requirements aren't implemented in Phase 0. They're recorded so Phase 0's design doesn't block them, and the M4 final review checks that it doesn't. A Meshmixer successor needs direct surface editing as well as repair.

**MF-SCULPT-001: Brushes.**
- **Brush set:** smooth, inflate/deflate, flatten, drag/move, pinch, crease, and carve (displace inward along the normal).
- **Brush settings:** radius (screen or world), strength, falloff curve, symmetry (X/Y/Z, with a pivot), and pen pressure where available.
- **Masking:** a freeze mask, plus the option to limit a brush to the current selection.

**MF-SCULPT-002: Detail where it's needed.** Local, dynamic re-triangulation under the brush (split, collapse and flip within the brush footprint) keeps triangle density close to a target edge length. Nothing outside the footprint is touched (MF-OP-003 applies per stroke).

**MF-SCULPT-003: Strokes are commands.**
- A stroke (press to release) is one command. Its record holds the brush parameters and the sampled path in a remappable form (MF-HIST-002), never raw vertex positions.
- Provenance lists the moved, created and removed elements.
- One undo per stroke. Replay is Exact on the same build.

**MF-SCULPT-004: Responsiveness.**
- At 5M triangles, a dab affects the screen within one frame at ≥ 60 fps on the reference machine.
- Only the GPU chunks a stroke touches are re-uploaded (MF-RENDER-003).
- History of 1,000 strokes stays within 1.5× of one revision, plus the stroke footprints (MF-DOC-002).

**MF-SCULPT-005: Carving and booleans (later).** Carving by brush is MF-SCULPT-001. Boolean subtract/union with a tool mesh is a separate later requirement, and isn't required for Phase 1's first release.

## 11b. Phase 1+ requirements captured now: modelling tools, plugins and scripting

These aren't implemented in Phase 0 and aren't assigned to a phase yet. They're recorded so Phase 0's design doesn't block them; the M4 and M5 reviews check that it doesn't.

Every tool below is a spec-driven operation like the repair tools (MF-SPEC-*). That means:
- one definition gives the GUI panel, the CLI command and the MCP schema;
- it is targetable where that makes sense;
- it records provenance;
- it is undoable, and replays Exact on the same build;
- it never silently falls back.

Printing-oriented parameters, such as clearances, are in document units.

**MF-OBJ-001: Object transform gizmo.**
- Move, rotate and scale whole objects in the viewport: drag handles, numeric entry, snapping (grid, angle, to the build plate), and "drop to plate".
- Transforms are commands (undoable, replayable) and are stored in the document and in 3MF.

**MF-MODEL-001: Booleans.**
- Union, subtract and intersect between objects, or a selected part and an object.
- Robust on real-world input: exact or adaptive predicates for every orientation and intersection decision; open, non-manifold or self-intersecting inputs are handled or refused by name.
- The output is closed and manifold whenever the inputs are, and provenance says which input each output face came from.
- *Accept:* boolean results on the M4 messy-model collection are either valid or named refusals, and a 1M × 1M union completes interactively (target set when the phase is planned).

**MF-MODEL-002: Intersection analysis.**
- Detects where objects intersect each other and where an object intersects itself (BVH-accelerated; shares machinery with MF-ISSUE-049).
- The results show as an overlay and a defect list, with an obvious next action: union, subtract, or move apart.

**MF-MODEL-003: Split with alignment pegs and joints.**
- Cut an object with a plane, or with a drawn path/region, into parts that are each capped closed.
- Optionally add alignment features across the cut: pegs and matching holes (round, square or keyed) or a keyed/dovetail joint. Each has a size, a count or automatic placement, and a printing **clearance**.
- Parts stay as separate objects in the document and in 3MF.
- *Accept:* split parts re-assemble with the requested clearance, measured on the meshes, and each part is closed and manifold.

**MF-MODEL-004: Kitbashing (adding parts to a model).**
- A part library with built-in and user-added parts. Drag a part onto a model's surface, where it snaps to the surface normal; then place, rotate and scale it with the gizmo.
- Attach the part as a separate object, merge it by boolean union, or **blend** it with a smooth transition zone (in the spirit of Meshmixer's "Meshmix"; uses fairing from MF-OP-002).
- Parts carry their own snap points (attach face, orientation, default scale).

**MF-MODEL-005: Parametric connectors.**
- Generated, printing-aware joints placed on or between objects: print-in-place pin hinges, snap-fit hinges, clasps and latches, ball-and-socket joints, pins, and dovetails.
- Parameters include the size, the **clearance**, the rotation range where relevant, and the orientation relative to the build plate.
- A connector is placed like a kitbash part (MF-MODEL-004) and merged or cut into the models it joins (MF-MODEL-001/003).
- *Accept:* each connector type prints and moves as intended on a reference printer at the default clearance. This is a manual check, recorded with photos.

**MF-PLUG-001: Plugins.**
- A plugin adds operations, kitbash parts or connectors. Each operation is defined by the same spec-file format as built-in operations, so it gets a GUI panel, a CLI command and an MCP schema for free.
- **Sandboxed:** plugin code (for example WebAssembly) can't crash the app, read arbitrary files or reach the network without a permission the user grants.
- **Edits go through the engine:** plugins edit geometry only through the engine's edit interface, so provenance, undo, replay and `untouched_outside_target` hold for plugin operations too.
- **Versioned interface:** a stable, versioned plugin API. A plugin built for one API version keeps working, or is refused by name.
- Install, update, disable and remove from inside the app; each plugin shows its source and permissions.

**MF-SCRIPT-001: Scripting.**
- Automate any operation sequence from a script (Python first): open, inspect, select, run operations, export.
- Scripts reuse the same command layer, so a script's run produces an ordinary command log that replays Exact.
- Scripts run headless (CLI) or inside the app with progress and cancel.

## 12. Exit criteria

On the M2 Max dev machine (preliminary for the hardware targets):

1. Open a representative 1–5M-triangle damaged mesh.
2. Keep viewport navigation within the MF-RENDER-002 targets.
3. Run `fill_holes` in the background with progress and cancellation.
4. Undo and redo it.
5. Save the command log, reload it, and replay it with `Exact` verification.
6. Invoke the same operation from GUI and CLI, and generate its MCP schema, all from one definition.
7. Export valid 3MF and STL.
8. Pass all declarative tests on Windows, macOS and Linux.
9. **Interactive repair workflow:** open a real damaged scan; see its defects via overlays; select one specific hole (or island); repair only that, with `untouched_outside_target` verified; inspect the result; undo; change a parameter; repair again; export.

**Evidence per milestone:** a short screen recording, benchmark JSON (including device info) and a sample repaired model. Evidence goes in `artifacts/<milestone>/` (git-ignored) and is summarized in `docs/milestones/<milestone>.md` (committed).

## 13. Open decisions

| # | Question | Status |
|---|---|---|
| OD-1 | Project licence | **Decided:** MIT OR Apache-2.0 |
| OD-2 | Lower-spec reference hardware | **Open:** needs a real integrated-GPU laptop (Windows or Linux preferred) before MF-RENDER-002 can be declared met |
| OD-3 | Command log format | **Decided for Phase 0:** JSON Lines plus a manifest. *Evolves:* binary `.mfx` container in Phase 1 |
| OD-4 | Spec format | **Decided:** YAML, compiled in, JSON Schema validated |
| OD-5 | Will a commercial or proprietary distribution ever be offered? | **Open:** affects acceptance of LGPL/MPL dependencies. Until decided, treat as "yes" (strictest) |
