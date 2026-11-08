# MeshFixer Phase 0 Engineering Spec (v0.1)

Status: Draft · 2026-11-08

## Purpose

This spec exists only to **constrain and falsify the Phase 0 spike**. It turns the exploration in `docs/idea.md`, `docs/requirements.md`, `docs/architecture.md` and `docs/sdd.md` into a small number of testable contracts. If something isn't listed here, it isn't Phase 0 work (see §8, Non-goals).

Each requirement has an ID, a statement, and an acceptance check. "MUST" items block Phase 0 exit.

Phase 0 answers five questions:

1. Does the document → command → history architecture work end to end (execute, undo, redo, serialize, replay)?
2. Are geometry operations deterministic enough to test with declarative postconditions?
3. Can one operation definition drive GUI, CLI, MCP schema, validation, serialization and tests?
4. Does the app stay interactive on large, *damaged* geometry while work runs in the background?
5. What happens to element identity when an operation changes topology, and is that recorded explicitly?

## 1. Settled decisions

| Decision | Phase 0 choice |
|---|---|
| Language | Rust (stable toolchain) |
| Desktop UI | egui (eframe) |
| Rendering | wgpu, in a crate independent of egui |
| Geometry core | Rust-native algorithms behind module boundaries |
| Import/export | STL (binary + ASCII), OBJ (geometry only), 3MF (core spec) |
| Primary interchange | 3MF |
| STEP / B-rep | Out of scope |
| Scripting, plugins | None |
| Automation | CLI first; MCP generated from the same operation definitions |
| History persistence | Serialized command log + optional checkpoints |
| Geometry state | Logically immutable snapshots, structurally shared storage |
| Operation definitions | Versioned declarative spec files (`specs/operations/*.yaml`) |
| Background processing | Mandatory for all geometry operations |
| Platforms | Windows, macOS, Linux from day one (CI builds all three) |

## 2. Workspace layout

```text
meshfixer/
  crates/
    mf-geometry    # mesh primitives, topology queries, attributes, element IDs
    mf-io          # STL / OBJ / 3MF readers and writers
    mf-document    # document, objects, revisions/snapshots
    mf-spec        # operation spec loading, parameter schema, validation
    mf-command     # command execution, undo/redo, command log, replay
    mf-operations  # inspect, remove_small_components, fill_holes, (simplify)
    mf-selection   # selection representation + provenance mapping
    mf-runtime     # jobs, cancellation, progress, thread/memory budgets
    mf-render      # GPU buffers, camera, picking, offscreen rendering
    mf-cli
    mf-mcp         # late Phase 0
    mf-desktop
  specs/operations/
  fixtures/
  tests/geometry/
```

Dependency direction: `mf-geometry` depends on nothing in the workspace. `mf-desktop`, `mf-cli` and `mf-mcp` are leaves. Only `mf-desktop` may depend on egui.

---

## 3. Contract A: Geometry and document state

**MF-GEO-001: Mesh representation.** An object's mesh is an indexed triangle mesh: `f32` vertex positions, `u32` triangle indices, stored in chunked, reference-counted buffers. Calculations that need more precision (volume, area, plane fits) use `f64` internally.
*Accept:* A 5M-triangle mesh fits in ≤ 400 MB resident, including derived topology.

**MF-GEO-002: Derived topology is lazy and cached.** Adjacency, boundary loops, connected components and non-manifold edges are computed on demand and cached per snapshot. They are never stored as authoritative state.
*Accept:* Loading a mesh doesn't compute adjacency. A second topology query on the same snapshot returns the cached result.

**MF-GEO-003: Diagnostics.** `mesh.inspect` reports at least: triangle count, vertex count, connected components, boundary edges, boundary loops, non-manifold edges, non-manifold vertices, degenerate triangles, duplicate vertices (exact and within tolerance), inconsistent orientation, bounding box, surface area, signed volume (only when closed), and whether the mesh is closed and manifold.
*Accept:* Each metric has a fixture with a known expected value.

**MF-DOC-001: Logically immutable snapshots.** Every revision of the document is an immutable snapshot. Executing an operation produces a new snapshot and never mutates an existing one.
*Accept:* After an operation, the previous snapshot's content hash is unchanged.

**MF-DOC-002: Structural sharing.** Snapshots share unchanged buffers or chunks. Storing full geometry for every revision isn't allowed. The memory model is copy-on-write chunks and may later add deltas, compressed checkpoints, or regeneration by replay.
*Accept:* Ten successive `remove_small_components` revisions on a 5M-triangle mesh use < 2× the memory of a single snapshot.

**MF-DOC-003: Content hashing.** Every snapshot has a deterministic content hash over its geometry and attributes.
*Accept:* Identical geometry produced by two different routes gives the same hash.

**MF-DOC-004: Multi-object documents.** A document holds an ordered list of named objects, each with a transform and a mesh. Document units default to millimetres and are stored explicitly.
*Accept:* A 3MF file with three objects round-trips with names, transforms and units preserved.

## 4. Contract B: Commands and history

**MF-CMD-001: Single mutation path.** All document changes go through `mf-command`. There's no mutation API reachable from `mf-desktop`, `mf-cli` or `mf-mcp` that bypasses it.
*Accept:* Enforced by crate visibility. A test confirms the frontends depend only on the command API for changes.

**MF-CMD-002: Command record.** A command record contains: operation ID, operation version, target (object ID and optional selection), validated parameters, input snapshot hash, output snapshot hash, engine version, duration, and status (`ok`, `failed`, `cancelled`).
*Accept:* Serializes to JSON and parses back without loss.

**MF-CMD-003: Undo/redo.** Undo and redo move between snapshots without recomputation. A new command after an undo discards the redo branch. (Branching history is a non-goal.)
*Accept:* Execute A, B, C, then undo ×2, redo ×1. The current hash equals the hash after B.

**MF-CMD-004: Replay.** A command log, together with its source files (referenced by content hash), can be replayed headlessly to reproduce the final document.
*Accept:* Replaying the log via CLI reproduces the final snapshot hash exactly. A source-hash mismatch is a hard error, never silently skipped.

**MF-CMD-005: Determinism.** For a given input snapshot, parameters and engine version, an operation produces bit-identical output regardless of thread count or platform.
*Accept:* CI runs each geometry test with 1 thread and with N threads on all three OSes and compares output hashes.

**MF-CMD-006: Failure is explicit.** A failed or cancelled operation produces no new snapshot, leaves the document unchanged, and returns a structured error (code, message, optional element references). No partial results and no silent fallbacks.
*Accept:* Fault-injection tests confirm the document hash is unchanged after failure and after cancellation.

**MF-CMD-007: Provenance map.** Every topology-changing operation returns a provenance map from input to output elements: kept (old ID → new ID), removed, and created (with the source elements, e.g. the boundary loop a patch filled). Phase 0 records this. It doesn't yet use it to remap selections.
*Accept:* For `remove_small_components` and `fill_holes`, a test checks that every output face is accounted for exactly once as kept or created, and every input face exactly once as kept or removed.

**MF-SEL-001: Selection identity.** A selection is a set of element IDs bound to one snapshot hash. If a selection is applied to a different snapshot, it is remapped through the provenance maps or rejected with an explicit error. It is never applied silently.
*Accept:* A face selection made before `fill_holes` remaps correctly afterwards. A selection on a removed component is reported as invalid, not dropped.

## 5. Contract C: Operation specification

**MF-SPEC-001: Declarative definitions.** Each operation is defined in `specs/operations/<id>.yaml` with: `id`, `version`, `summary`, `target` (accepted kinds), `parameters` (type, default, range, units, description), `preconditions`, `postconditions`, `effects` (e.g. `topology_changes`), and runtime traits (`cancellable`, `progress`).
*Accept:* The spec files validate against a JSON Schema checked in under `specs/`.

**MF-SPEC-002: Spec ↔ implementation parity.** Every spec has exactly one registered implementation, and every implementation has a spec. The spec version must match the implementation version.
*Accept:* A unit test fails on any missing or mismatched pair.

**MF-SPEC-003: One definition, every interface.** From the operation spec alone, the system produces: GUI parameter controls, CLI subcommand arguments, MCP tool JSON Schema, parameter validation, and command-log serialization. No frontend may hand-write parameter definitions for an operation.
*Accept:* Adding a parameter to `fill_holes.yaml` (plus its use in the implementation) makes it appear in the GUI panel, `meshfixer fill-holes --help` and the MCP tool schema with no frontend code changes.

**MF-SPEC-004: Declarative geometry tests.** Test cases under `tests/geometry/*.yaml` take the form *given fixture → when operation(params) → expect postconditions*, using diagnostic metrics from MF-GEO-003 (e.g. `boundary_loops: 0`, `connected_components: 1`, `volume_delta_pct_lt: 1.0`).
*Accept:* The test runner executes all cases via `cargo test` and reports per-case failures.

**MF-SPEC-005: Phase 0 operations.** Implement these operations in this order:
1. `mesh.inspect` (read-only)
2. `mesh.remove_small_components` (topology deletion, by triangle count and/or area threshold)
3. `mesh.fill_holes` (topology creation, with a maximum hole size and a minimal-area or fan triangulation; fairing is optional)
4. `mesh.simplify` (stretch goal, the first heavy stress test)

*Accept:* Items 1–3 pass their declarative tests. Item 4 is not required for exit.

## 6. Contract D: Runtime and resources

**MF-RT-001: No geometry on the UI thread.** The UI thread never runs an operation, an import/export, or a topology computation over O(n) on mesh size.
*Accept:* With a debug instrumentation flag, a frame-time trace shows no UI frame over 50 ms while `fill_holes` runs on a 5M-triangle mesh.

**MF-RT-002: Cancellation.** Every long operation and import accepts a cancellation token and checks it at least every 100 ms of work.
*Accept:* Cancelling within 250 ms of the request is measured in tests. The document stays unchanged (MF-CMD-006).

**MF-RT-003: Progress.** Long operations report monotonically increasing progress (0–1) with a stage label.
*Accept:* The GUI and the CLI (with `--progress`) both display progress for import and `fill_holes`.

**MF-RT-004: Thread budget.** Background work runs on a dedicated pool. By default it leaves at least one core free for UI and rendering, and the size is configurable.
*Accept:* The pool size is visible in diagnostics. Viewport frame rate during background work stays within the MF-RENDER-002 targets.

**MF-RT-005: Telemetry.** The app records operation durations, peak resident memory, and frame-time percentiles (p50, p95, p99). They're shown in a debug panel and written by CLI `--metrics`.
*Accept:* The metrics output is JSON and used by benchmark CI.

## 7. Contract E: Rendering and UI separation

**MF-RENDER-001: Renderer independence.** `mf-render` doesn't depend on egui and can render offscreen to an image headlessly.
*Accept:* CLI `meshfixer render in.stl -o out.png` works without a window and is used by snapshot tests.

**MF-RENDER-002: Interactive targets.** On the reference machine (16 GB RAM, integrated GPU):
- 1M triangles: ≥ 60 fps orbit/pan/zoom
- 5M triangles: ≥ 30 fps
- 10M triangles: loads and stays navigable (≥ 15 fps, or LOD allowed)

*Accept:* An automated orbit benchmark reports frame-time percentiles per size. The damaged-mesh fixture is used, not a clean one.

**MF-RENDER-003: Incremental GPU updates.** A selection change updates only selection state (e.g. a per-face flag buffer), never geometry buffers. A geometry change re-uploads only changed chunks.
*Accept:* Instrumented upload byte counts confirm both.

**MF-RENDER-004: Picking.** Face picking returns the face ID under the cursor in < 16 ms at 5M triangles.
*Accept:* Benchmark.

**MF-RENDER-005: Diagnostics overlay.** The viewport can highlight boundary edges, non-manifold edges and small components from `mesh.inspect`.
*Accept:* Visual snapshot test via offscreen rendering.

## 8. I/O, platform and legal

**MF-IO-001: Formats.** Read and write STL (binary and ASCII), OBJ (`v`/`f` only; other lines ignored with a warning), and 3MF core (multiple objects, transforms, units). Readers stream and report progress.
*Accept:* Round-trip tests per format. Exported 3MF validates against the 3MF core spec and opens in at least one mainstream slicer (manual check).

**MF-IO-002: Hostile input.** Malformed, truncated or adversarial files produce structured errors, never panics, unbounded allocation, or hangs.
*Accept:* A fuzz target per reader runs in CI (time-boxed).

**MF-PLAT-001: Cross-platform CI.** Every commit builds and runs the test suite on Windows, macOS and Linux.
*Accept:* A GitHub Actions matrix on all three. Platform-specific code lives only in `mf-desktop` and clearly marked adapter modules.

**MF-ARCH-LEGAL-001: Licence discipline.** No mandatory dependency may impose a copyleft obligation incompatible with the project licence. Optional dependencies with stronger licences must sit behind a replaceable boundary and be reviewed before adoption.
*Accept:* `cargo deny` runs in CI with an allowlist of licences.

## 9. Non-goals for Phase 0

STEP/B-rep, fillets/chamfers, remeshing, smoothing, booleans, point clouds, textures, surface text, pattern/feature recognition, tags, branching or editable-parameter history, plugins, scripting, LLM planning, cloud anything, installers and signing, and a polished UI. The architecture shouldn't block these, but none of them may shape Phase 0 APIs beyond what this spec requires.

## 10. Fixtures

- **Procedural generator** (`fixtures/gen`): a deterministic, seeded generator for clean primitives and controlled damage (holes of a given size, islands, duplicate vertices, degenerate and flipped triangles, non-manifold fins), scalable to 1M, 5M and 10M triangles. Large fixtures are generated at test time, not checked in.
- **Small hand-made fixtures:** `cube_clean`, `cube_hole`, `cube_nonmanifold`, `multi_shell`, `flipped_normals`, `degenerate`, all checked in.
- **Real scans:** at least one permissively licensed photogrammetry or phone-scan mesh (1–5M triangles), source and licence recorded in `fixtures/SOURCES.md`.

## 11. Phase 0 exit criteria

On the reference 16 GB laptop, MeshFixer can:

1. open a representative 1–5M-triangle damaged mesh,
2. keep viewport navigation interactive (MF-RENDER-002),
3. run `fill_holes` in the background with progress and cancellation,
4. undo and redo it,
5. save the command log, reload it, and replay it to an identical hash,
6. invoke the same operation from GUI and CLI using one operation definition,
7. export valid 3MF and STL,
8. pass all declarative geometry tests on Windows, macOS and Linux.

If any of these fail, the corresponding architectural bet gets revisited before Phase 1 work begins.

## 12. Open decisions

| # | Question | Proposed default |
|---|---|---|
| OD-1 | Project licence | MIT OR Apache-2.0 (Rust ecosystem norm) |
| OD-2 | Reference hardware for MF-RENDER-002 | A specific integrated-GPU laptop to be named; until then, an Apple M-series Mac with the thread pool and memory capped to simulate it |
| OD-3 | Command log format | JSON Lines (`.mfx.log`) plus a manifest. A binary `.mfx` container comes in Phase 1 |
| OD-4 | Spec format | YAML authored, compiled into the binary, JSON Schema validated |
