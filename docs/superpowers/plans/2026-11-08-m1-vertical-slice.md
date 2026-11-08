# M1 Thin Vertical Slice Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Open a damaged mesh in a desktop app, see diagnostics and boundary overlays, and run a spec-defined operation from both GUI and CLI with undo, all through one command path.

**Architecture:** A Cargo workspace of small crates that only depend downward: geometry → io/spec → operations → command → render → frontends. Operations are defined in YAML and registered in Rust. A parity test keeps the two in sync. The GUI prepares a command on the UI thread, runs it on a worker thread, and commits it back.

**Tech Stack:** Rust stable, eframe/egui 0.37 (wgpu backend), wgpu (the version eframe re-exports), glam, blake3, serde + serde_json + serde-saphyr (YAML), clap 4 (builder API), thiserror 2, bytemuck, image, pollster, rfd.

**Spec:** `docs/spec/phase0-spec.md` (v0.2); milestones in `docs/plan/phase0-milestones.md`.

## Global Constraints

- Licence: MIT OR Apache-2.0. Every crate sets `license = "MIT OR Apache-2.0"`, and `cargo deny check licenses` must pass.
- Working positions are `[f64; 3]`, indices `[u32; 3]`. GPU uses `f32` relative to the bounding-box centre. [MF-GEO-001]
- `mf-geometry` has no workspace dependencies. Only `mf-desktop` depends on egui/eframe. `mf-render` has no egui dependency. [§2]
- No `unwrap()`/`expect()` on data derived from input files. Errors are structured `thiserror` enums. No silent fallbacks. [MF-CMD-006, MF-IO-002]
- Operations never mutate their input. They return a new `Mesh`. [MF-DOC-001]
- CI matrix: `ubuntu-latest`, `macos-latest`, `windows-latest`. [MF-PLAT-001]

## Review Focus

1. **Empty or zero-triangle files:** STL with 0 facets, or OBJ with only `v` lines, must import as an empty mesh; inspect reports zeros and the viewport doesn't crash. Test: `stl::tests::zero_facets`, `render` offscreen with an empty mesh.
2. **Binary STL whose header starts with `solid`:** many exporters do this. Detect by size (`84 + 50·n == len`), not by prefix. Test: `stl::tests::binary_with_solid_header`.
3. **OBJ negative/relative indices and `f v/vt/vn` forms:** must parse. Out-of-range indices are an error naming the line. Test: `obj::tests::relative_and_slash_indices`, `obj::tests::out_of_range_index_errors`.
4. **Removing every component:** `remove_small_components` on a mesh where all components are small must keep the largest when `keep_largest=true`. With `keep_largest=false` it may produce an empty mesh, which is valid. Test: `remove_small::tests::keep_largest_guard`.
5. **Commit after the document moved on:** if the user undoes while a job runs, committing a result whose input revision is no longer current must return `CommandError::StaleInput` and leave the document unchanged. Test: `session::tests::stale_commit_rejected`.

---

## File structure

```text
Cargo.toml                         # workspace, shared deps, lints
deny.toml                          # licence allowlist
.github/workflows/ci.yml
specs/operations/mesh.inspect.yaml
specs/operations/mesh.remove_small_components.yaml
specs/operation.schema.json
crates/mf-geometry/src/{lib.rs, mesh.rs, hash.rs, topology.rs, diagnostics.rs}
crates/mf-fixtures/src/lib.rs      # procedural meshes (dev-dependency only)
crates/mf-io/src/{lib.rs, stl.rs, obj.rs}
crates/mf-spec/src/{lib.rs, model.rs, params.rs, builtin.rs, cli.rs}
crates/mf-operations/src/{lib.rs, inspect.rs, remove_small.rs}
crates/mf-command/src/{lib.rs, document.rs, session.rs}
crates/mf-render/src/{lib.rs, camera.rs, gpu_mesh.rs, renderer.rs, offscreen.rs, shader.wgsl}
crates/mf-cli/src/main.rs
crates/mf-desktop/src/{main.rs, app.rs, viewport.rs, panels.rs}
```

---

### Task 1: Workspace, CI and licence gate

**Files:** Create `Cargo.toml`, `deny.toml`, `.github/workflows/ci.yml`, `rust-toolchain.toml` (channel `stable`), `README.md`. Each crate gets an empty `lib.rs`/`main.rs` and its own `Cargo.toml` with `version.workspace = true`, `license.workspace = true`, `edition = "2024"`.

- [ ] **Step 1:** Workspace `Cargo.toml` with `[workspace.package] version = "0.0.1"`, `license = "MIT OR Apache-2.0"`, `edition = "2024"`, `[workspace.dependencies]` for every crate in the Tech Stack, and `[workspace.lints.rust] unsafe_code = "forbid"`. `mf-render` and `mf-desktop` override this to `deny` because bytemuck/wgpu need it.
- [ ] **Step 2:** `deny.toml` allows `MIT`, `Apache-2.0`, `Apache-2.0 WITH LLVM-exception`, `BSD-2-Clause`, `BSD-3-Clause`, `ISC`, `Zlib`, `Unicode-3.0`, `BSL-1.0`, `CC0-1.0`, `MPL-2.0` (file-level copyleft is compatible with MIT/Apache distribution; flagged for OD-5 review).
- [ ] **Step 3:** CI jobs `test` (matrix of 3 OSes: `cargo fmt --check`, `cargo clippy --workspace -- -D warnings`, `cargo test --workspace`) and `deny` (ubuntu, `EmbarkStudios/cargo-deny-action@v2`, `check licenses`).
- [ ] **Step 4:** Run `cargo build --workspace && cargo deny check licenses`. Expected: both succeed.
- [ ] **Step 5:** Commit `chore: workspace scaffold, CI matrix, licence gate [MF-PLAT-001, MF-ARCH-LEGAL-001]`.

### Task 2: Mesh, content hash and procedural fixtures

**Files:** `mf-geometry/src/{mesh.rs, hash.rs}`, `mf-fixtures/src/lib.rs`.

**Interfaces (produces):**
- `Mesh::new(positions: Vec<[f64;3]>, triangles: Vec<[u32;3]>) -> Result<Mesh, GeometryError>`. Validates indices `< positions.len()` and all coordinates finite. `Mesh` is `Clone` (cheap; `Arc` buffers), `Send + Sync`.
- `Mesh::empty()`, `positions(&self) -> &[[f64;3]]`, `triangles(&self) -> &[[u32;3]]`, `vertex_count()`, `triangle_count()`, `bounding_box(&self) -> Option<Aabb>` (`Aabb { min: [f64;3], max: [f64;3] }`, with `center()`).
- `Mesh::content_hash(&self) -> ContentHash` (`ContentHash(pub [u8;32])`, `Display` as hex). BLAKE3 over: `b"mf-mesh-v1"`, vertex count u64 LE, positions as f64 LE, triangle count u64 LE, indices u32 LE. Cached in a `OnceLock`.
- `GeometryError::{IndexOutOfRange { triangle: usize, index: u32 }, NonFinite { vertex: usize }}`.
- `mf_fixtures::{cube(size: f64) -> Mesh, cube_with_missing_face() -> Mesh, translated(mesh: &Mesh, d: [f64;3]) -> Mesh, merge(meshes: &[Mesh]) -> Mesh, tetrahedron() -> Mesh, flipped_tetrahedron() -> Mesh, uv_sphere(rings: u32, segments: u32) -> Mesh}`. The cube is 8 vertices and 12 triangles, outward CCW.

- [ ] **Step 1: Tests**: `new_rejects_out_of_range` (index 3 with 3 vertices → `IndexOutOfRange{triangle:0,index:3}`), `new_rejects_nan`, `hash_stable` (cube hash equals itself recomputed, ≠ `translated(cube, [1e-9,0,0])` hash), `hash_order_sensitive` (swapping two triangles changes the hash), `bbox_cube` (min `[0,0,0]`, max `[1,1,1]`).
- [ ] **Step 2:** `cargo test -p mf-geometry`. Expected: compile FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Tests PASS.
- [ ] **Step 5:** Commit `feat(geometry): immutable f64 mesh with canonical content hash [MF-GEO-001, MF-DOC-003]`.

### Task 3: Lazy topology and diagnostics

**Files:** `mf-geometry/src/{topology.rs, diagnostics.rs}`.

**Interfaces (produces):**
- `Mesh::topology(&self) -> Arc<Topology>`, computed once per mesh (`OnceLock<Arc<Topology>>`) [MF-GEO-003].
- `Topology { components: Vec<Vec<u32>> /* face ids, sorted, largest first; tie → lowest first face */, boundary_loops: Vec<BoundaryLoop>, non_manifold_edges: Vec<[u32;2]>, inconsistent_edges: usize }`.
- `BoundaryLoop { id: u32, vertices: Vec<u32> /* ordered */, edge_count: usize }`. Loops are found by walking boundary half-edges. Loops are only traced through manifold boundary vertices; boundary edges at non-manifold vertices are counted in `boundary_edges` but excluded from loops, and reported in `Diagnostics::untraced_boundary_edges`.
- `Diagnostics` (serde `Serialize`): `triangle_count, vertex_count, components, boundary_edges, boundary_loops: Vec<LoopSummary{id, edge_count, perimeter, centroid}>, untraced_boundary_edges, non_manifold_edges, non_manifold_vertices, degenerate_triangles /* area < 1e-12·bbox_diag² */, duplicate_vertices_exact, inconsistent_orientation_edges, bounding_box, surface_area, signed_volume: Option<f64> /* only if closed && manifold */, is_closed, is_manifold`.
- `diagnostics::inspect(mesh: &Mesh) -> Diagnostics`.

Components connect through shared edges (not shared vertices only). An edge is non-manifold when it has more than 2 incident faces. An edge with 2 faces traversing it in the same direction counts as inconsistent.

- [ ] **Step 1: Tests**: `cube_closed` (components 1, loops 0, boundary_edges 0, is_closed, signed_volume ≈ 1.0 ± 1e-12, area 6.0), `missing_face` (1 loop with edge_count 4, perimeter 4.0, signed_volume None), `two_cubes` (components 2, each with 12 faces), `flipped` (inconsistent_orientation_edges 3), `fin` (three triangles sharing one edge → non_manifold_edges 1), `topology_cached` (`Arc::ptr_eq` across two calls), `empty_mesh` (all zeros, no panic).
- [ ] **Step 2:** Run. FAIL.
- [ ] **Step 3:** Implement with a sorted edge list (`Vec<(u32,u32,u32 face)>` sorted by undirected key). No HashMap, for determinism.
- [ ] **Step 4:** PASS.
- [ ] **Step 5:** Commit `feat(geometry): lazy topology and inspect diagnostics [MF-GEO-003, MF-GEO-004]`.

### Task 4: STL and OBJ I/O

**Files:** `mf-io/src/{lib.rs, stl.rs, obj.rs}`.

**Interfaces (produces):**
- `Format::{StlBinary, StlAscii, Obj}`, `Format::from_path(&Path) -> Option<Format>` (`.stl` → StlBinary for writing).
- `SourceInfo { path: PathBuf, content_hash: ContentHash /* BLAKE3 of file bytes */, format: Format, precision: SourcePrecision::{F32, Decimal{max_significant_digits: u8}} }`.
- `Imported { mesh: Mesh, source: SourceInfo, warnings: Vec<String> }`.
- `read_path(path: &Path) -> Result<Imported, IoError>`, `write_path(mesh: &Mesh, path: &Path, format: Format) -> Result<(), IoError>`.
- `stl::read(bytes: &[u8]) -> Result<(Mesh, SourcePrecision), IoError>`. Detects binary by `len == 84 + 50·n`. Welds exactly-equal f32 positions with a sorted dedupe (deterministic). Rejects `n` implying more than `len` bytes before allocating.
- `obj::read(text: &str) -> Result<(Mesh, SourcePrecision, Vec<String>), IoError>`. Handles `v`, `f` with `a`, `a/b`, `a//c` and `a/b/c`, negative indices, and fan-triangulates polygons. Other directives produce one aggregated warning per keyword.
- `obj::write` uses `{}` formatting of f64 (shortest round-trip representation).
- `IoError::{Io(std::io::Error), Parse { line: usize, message: String }, Truncated { expected: usize, actual: usize }, Geometry(GeometryError), UnsupportedFormat(PathBuf)}`.

- [ ] **Step 1: Tests**: `stl_binary_roundtrip` (cube → bytes → mesh has 8 vertices, 12 triangles), `stl_ascii_parse`, `binary_with_solid_header`, `zero_facets`, `truncated_errors` (claims 10 facets with 3 present → `Truncated`), `obj_roundtrip_precision` (vertex `0.123456789012345` survives within 1e-15 relative), `relative_and_slash_indices`, `out_of_range_index_errors` (→ `Parse{line:..}`), `quad_triangulated`.
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(io): STL and OBJ readers/writers [MF-IO-001, MF-GEO-001]`.

### Task 5: Operation specs and parameter validation

**Files:** `specs/operation.schema.json`, `specs/operations/*.yaml`, `mf-spec/src/{model.rs, params.rs, builtin.rs, cli.rs}`.

**Interfaces (produces):**
- `OperationSpec { id: String, version: u32, summary: String, geometry_kinds: Vec<String>, target: TargetSpec { kinds: Vec<String>, whole_object: bool }, parameters: Vec<ParamSpec>, preconditions: Vec<String>, postconditions: Vec<String>, effects: Vec<String>, runtime: RuntimeSpec { cancellable: bool, progress: bool } }` (serde `Deserialize`, `Serialize`).
- `ParamSpec { name: String, kind: ParamKind, unit: Option<String>, description: String }` with `ParamKind::{Integer{default:i64,min:Option<i64>,max:Option<i64>}, Float{default:f64,min:Option<f64>,max:Option<f64>}, Bool{default:bool}}` (serde-tagged `type: integer|float|bool`).
- `ParamValue::{Int(i64), Float(f64), Bool(bool)}`. `Params(BTreeMap<String, ParamValue>)` with `get_int/get_float/get_bool(&self, name) -> Result<_, SpecError>`.
- `OperationSpec::validate(&self, raw: &BTreeMap<String, String>) -> Result<Params, SpecError>`. Parses strings, fills defaults, enforces ranges, and rejects unknown names.
- `builtin::specs() -> &'static [OperationSpec]` (a `OnceLock`, parsed from `include_str!`). `builtin::get(id) -> Option<&'static OperationSpec>`.
- `cli::to_command(spec: &OperationSpec) -> clap::Command` (subcommand name is the id after `mesh.` with `_` → `-`; one `--param-name` arg per parameter using the description as help) and `cli::raw_params(spec, &ArgMatches) -> BTreeMap<String,String>`.
- `SpecError::{UnknownParameter(String), InvalidValue{name,value,reason}, OutOfRange{name,value,min,max}, UnknownOperation(String)}`.

`mesh.remove_small_components.yaml` parameters: `max_triangles` (integer, default 100, min 1; "Remove components with fewer triangles than this") and `keep_largest` (bool, default true). `mesh.inspect.yaml` has no parameters. Both use `geometry_kinds: [mesh]`, `target.whole_object: true`, `target.kinds: []` in M1.

- [ ] **Step 1: Tests**: `builtin_specs_validate_against_schema` (jsonschema as a dev-dependency), `defaults_filled`, `out_of_range_rejected` (`max_triangles=0` → `OutOfRange`), `unknown_param_rejected`, `cli_args_match_spec` (`to_command` for remove_small has args `max-triangles` and `keep-largest`, and the help text equals the spec description).
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(spec): declarative operation specs, validation, CLI derivation [MF-SPEC-001, MF-SPEC-003]`.

### Task 6: Operations and parity

**Files:** `mf-operations/src/{lib.rs, inspect.rs, remove_small.rs}`.

**Interfaces (produces):**
- `trait Operation: Send + Sync { fn id(&self) -> &'static str; fn version(&self) -> u32; fn execute(&self, input: &Mesh, params: &Params) -> Result<OpOutput, OpError>; }`
- `OpOutput { mesh: Option<Mesh> /* None = read-only */, report: serde_json::Value }`.
- `OpError::{Spec(SpecError), Failed { code: &'static str, message: String }}`.
- `registry() -> &'static [&'static dyn Operation]`, `find(id) -> Option<&'static dyn Operation>`.
- `remove_small_components` keeps kept faces in their original relative order. Vertices are compacted in first-use order. The report has `{removed_components, removed_triangles}`.

- [ ] **Step 1: Tests**: `parity_specs_and_impls` (same ID sets, same versions) [MF-SPEC-002], `removes_island` (merged cube + translated tetrahedron, `max_triangles=5` → 12 triangles, 1 component, the output positions equal the cube's positions in order), `keep_largest_guard`, `input_not_mutated` (input hash unchanged), `inspect_is_read_only` (`mesh: None`, and the report deserializes to the diagnostics fields).
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(operations): inspect and remove_small_components with parity test [MF-OP-001, MF-SPEC-002]`.

### Task 7: Document and session (prepare/run/commit, undo/redo)

**Files:** `mf-command/src/{document.rs, session.rs}`.

**Interfaces (produces):**
- `ObjectId(u64)` is allocated from a monotonic counter and never reused. `Object { id, name: String, mesh: Mesh, source: Option<SourceInfo> }`. `Document { objects: Vec<Object> }` (Clone, cheap). The `objects()` accessor is read-only, and mutation methods are `pub(crate)`.
- `Invocation { op_id: String, target: ObjectId, raw_params: BTreeMap<String,String> }`.
- `Session::new() -> Session`, `Session::import(&mut self, imported: Imported, name: String) -> ObjectId` (itself a history entry), `document(&self) -> &Document`, `revision(&self) -> u64` (the current history index).
- `Session::prepare(&self, inv: Invocation) -> Result<Prepared, CommandError>`. Validates params via the spec and resolves the target. `Prepared` is `Send` and holds the cloned input `Mesh`, `Params`, `&'static dyn Operation`, and `base_revision`.
- `Prepared::run(self) -> Result<Completed, CommandError>`, safe on any thread. `Completed { base_revision, target, op_id, params, output: OpOutput, duration: Duration }`.
- `Session::commit(&mut self, c: Completed) -> Result<CommitOutcome, CommandError>`. `StaleInput` if `c.base_revision != self.revision()`. A read-only output records nothing in history and returns `CommitOutcome::Report(Value)`; otherwise `CommitOutcome::NewRevision { report }`.
- `Session::execute(&mut self, inv) -> Result<CommitOutcome, CommandError>` = prepare + run + commit (CLI/tests).
- `undo(&mut self) -> bool`, `redo(&mut self) -> bool`, `can_undo/can_redo`, `history_labels(&self) -> Vec<String>`.
- `CommandError::{Spec(SpecError), UnknownOperation(String), UnknownObject(ObjectId), Operation(OpError), StaleInput { expected: u64, actual: u64 }}`.

- [ ] **Step 1: Tests**: `execute_undo_redo` (import, remove ×1, undo → hash equals the import hash, redo → hash equals the post-op hash), `new_command_after_undo_truncates_redo`, `failed_op_leaves_document` (invalid param → error, revision unchanged), `stale_commit_rejected`, `read_only_does_not_create_revision`.
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(command): session with prepare/run/commit and linear undo [MF-CMD-001, MF-CMD-003, MF-CMD-006]`.

### Task 8: CLI

**Files:** `mf-cli/src/main.rs` (binary name `meshfixer`).

**Interfaces:**
- `meshfixer inspect <in> [--json]`
- `meshfixer <op-subcommand> <in> -o <out> [spec-derived args]`: one subcommand per mutating builtin spec, built via `mf_spec::cli::to_command`
- `meshfixer render <in> -o <png> [--width 1024 --height 768]` (added in Task 9)

Exit code 0 on success, 2 on usage/spec errors, 1 on I/O or operation errors. Errors go to stderr.

- [ ] **Step 1: Tests** (`tests/cli.rs` using `std::process::Command` with `env!("CARGO_BIN_EXE_meshfixer")`): `inspect_json_reports_components` (writes the merged fixture to a temp file and checks `"components":2`), `remove_small_writes_output` (output has 1 component), `help_lists_spec_params` (`remove-small-components --help` contains the spec description text), `bad_param_exit_code_2`.
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(cli): spec-derived subcommands sharing the session path [MF-SPEC-003, MF-CMD-001]`.

### Task 9: Renderer with offscreen output and boundary overlay

**Files:** `mf-render/src/*`.

**Interfaces (produces):**
- `OrbitCamera { target: glam::DVec3, distance: f64, yaw: f64, pitch: f64, fov_y: f64 }` with `fit(aabb: &Aabb) -> Self`, `orbit(dx, dy)`, `pan(dx, dy)`, `zoom(factor)`, `view_proj(aspect: f32, origin: DVec3) -> glam::Mat4`. View-projection is computed in f64 relative to the mesh's local origin, then cast.
- `GpuMesh::upload(device, &Mesh) -> GpuMesh`. Non-indexed flat-shaded vertices `{pos: [f32;3], normal: [f32;3]}` relative to `origin = aabb.center()`, plus a line buffer of boundary-loop edges from `mesh.topology()`. Fields: `origin: DVec3`, `triangle_count`.
- `MeshRenderer::new(device, color_format, depth_format) -> Self`, `MeshRenderer::prepare(queue, &GpuMesh, &OrbitCamera, aspect)`, `MeshRenderer::paint(&self, pass: &mut wgpu::RenderPass<'_>, &GpuMesh, show_boundaries: bool)`. Pipelines: shaded triangles (headlight Lambert) and boundary lines (solid orange, drawn with a depth bias towards the camera).
- `offscreen::render_png(mesh: &Mesh, width: u32, height: u32, show_boundaries: bool) -> Result<image::RgbaImage, RenderError>`. Creates its own headless device via `pollster`.

- [ ] **Step 1: Tests**: `camera_fit_contains_bbox` (all 8 bbox corners project into NDC [-1,1]), `offscreen_cube_not_blank` (the centre pixel differs from the clear colour; skipped with a printed notice if no adapter is available, which is the only allowed skip, for CI runners without a GPU), `offscreen_empty_mesh_ok`.
- [ ] **Step 2–4:** FAIL → implement → PASS. Add the `meshfixer render` subcommand.
- [ ] **Step 5:** Commit `feat(render): wgpu mesh renderer, orbit camera, offscreen PNG, boundary overlay [MF-RENDER-001, MF-RENDER-005]`.

### Task 10: Desktop app

**Files:** `mf-desktop/src/{main.rs, app.rs, viewport.rs, panels.rs}`.

**Behaviour:**
- File → Open (rfd) imports on a worker thread (`std::thread::spawn` + `std::sync::mpsc`). Optional CLI arg path. File → Export STL/OBJ.
- Central viewport: an `egui_wgpu::CallbackTrait` paint callback that uses `MeshRenderer`. Left-drag orbits, right-drag or shift-drag pans, scroll zooms, `F` fits.
- Right panel: Diagnostics (the `inspect` report, recomputed on a worker after each revision), an Operations list from `builtin::specs()` (mutating ones), and a parameter form generated from `ParamSpec` (`DragValue` with ranges for int/float, checkbox for bool) with an **Apply** button → `prepare` on the UI thread, `run` on a worker, `commit` on receipt. A spinner shows while running, and controls are disabled.
- History panel with Undo/Redo buttons plus Cmd/Ctrl+Z and Shift+Cmd/Ctrl+Z. A "Show boundaries" toggle.
- Errors appear in a dismissible red banner, never swallowed.

- [ ] **Step 1:** Implement. GUI code has no unit tests. Its logic lives in the tested `Session`.
- [ ] **Step 2:** `cargo run -p mf-desktop -- fixtures/generated/damaged.stl`. Verify manually: orbit, overlay, apply, undo, redo, export. Capture a screen recording to `artifacts/m1/`.
- [ ] **Step 3:** Commit `feat(desktop): egui shell with spec-generated operation panels and background execution [MF-SPEC-003, MF-RT-001]`.

### Task 11: M1 evidence and close-out

- [ ] **Step 1:** Add `meshfixer gen-fixture damaged <out.stl> --seed 1 --rings N`: a UV sphere with punched holes, plus 20 small tetra islands and duplicated vertices.
- [ ] **Step 2:** Produce `artifacts/m1/`: the recording, before/after STL, `inspect --json` before and after, and offscreen PNGs.
- [ ] **Step 3:** Write `docs/milestones/m1.md` with the gate table (G1.1–G1.6, pass/fail with evidence links), what was learned, and what's deliberately temporary.
- [ ] **Step 4:** Push CI and confirm green on all 3 OSes (needs a GitHub remote; if none, record the local results and note the CI status as pending).
- [ ] **Step 5:** Commit `docs: M1 milestone report`.
