# Phase 1 Modelling Tools Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** a modern Meshmixer that matches and then exceeds Meshmixer's main pieces: moving objects, booleans, splitting with alignment pegs, sculpting, mixing parts onto models ("Meshmix") and parametric connectors. Everything stays interactive at 5–30M triangles, every edit replays exactly, and nothing ever produces invalid geometry silently.

**Owner use cases (end-to-end acceptance).** Each one is an automated scenario test driven through commands or the CLI, plus a GUI frame capture, in the milestone that completes it:

| Use case | What it does | Completed in |
|---|---|---|
| **UC1: split and pegs** | Split a model horizontally, vertically and on a bias. Add alignment holes on both cut faces. Dowels are generated as separate objects, laid out on the plate, sized from the fit profile. | P1-M3, Task 16a |
| **UC2: "Rocktopus"** | Cut the head off an octopus and delete it. Bring in a new head, place it on the neck with the gizmo, join with a blended union, and touch up with sculpt brushes. Works whether the neck is capped or left open. | P1-M5, Task 24a |
| **UC3: hinged box** | Split the lid off a hollow box. Add two print-in-place hinges and a snap clasp from the library, resize and place them, and merge each leaf onto body or lid. | P1-M6, Task 30 |

**Architecture:**
- **One command contract for every tool.** Operations gain *operands*: other objects or selections, such as a boolean's tool or a peg. Their results are *object changes*: geometry, transform, object created, object removed. Provenance can name faces of any operand. Transform, boolean, split, sculpt stroke and part placement are all versioned, spec-driven operations, and connectors (MF-MODEL-005), plugins (MF-PLUG-001) and scripts (MF-SCRIPT-001) reuse the same contract.
- **The boolean kernel sits behind an adapter.** `mf-boolean` wraps the exact-rational mesh-arrangement engine of `manifold-rust`. Two pieces of our own code surround it:
  - a local window, so cost follows the size of the overlap rather than the size of the model;
  - an independent validator (exact predicates), so an invalid result is refused by name and never committed.
- **Our own exact geometry** (`robust` predicates) handles intersection detection, plane cuts and output validation. Cut caps are triangulated by a constrained Delaunay triangulation (`spade`).
- **The desktop stays target-first.** The Transform gizmo, Boolean, Split, Sculpt and Parts tools open from the target in Properties or directly in the viewport. Live interaction changes only GPU uniforms or the touched chunks. Commits run on workers.

**Tech Stack:** as M4 (Rust 1.99.1, eframe/egui 0.37, wgpu 30, rayon, faer for AMD ordering). The new dependencies are `manifold-rust =0.19.2`, `robust 1.2` and `spade 2.15` (see D1–D3).

**Spec:** `docs/spec/phase0-spec.md` v0.5: §1–8 contracts, §11a (MF-SCULPT-001…005) and §11b (MF-OBJ-001, MF-MODEL-001…005; MF-PLUG-001 and MF-SCRIPT-001 must not be blocked). The owner's use cases UC1–UC3 (above) are acceptance requirements. Roadmap: `docs/plan/phase0-milestones.md`, "After Phase 0". Task 2 writes `docs/spec/phase1-spec.md`, which turns the decisions below into numbered, testable requirements. Executors read both specs.

**Milestones and order:** P1-M1 transform gizmo → P1-M2 intersection and booleans → P1-M3 split, pegs and joints → P1-M4 sculpting → P1-M5 mixing (kitbash / Meshmix) → P1-M6 parametric connectors (MF-MODEL-005, brought forward from Phase 2 for UC3).

This follows the owner's order. Three dependencies confirm it:
- splitting with pegs needs booleans;
- Meshmix's blend reuses sculpting's local remesher (MF-SCULPT-002) and its region fairing;
- connectors are placed like parts (P1-M5's `SurfaceFrame` and `place`) and joined by union.

**UX checkpoints don't stop the work (owner, 2026-11-10).** At each checkpoint, agents capture frames and a recording into `artifacts/phase1/<milestone>/` and carry on. The owner reviews them all together later. The final demo (Task 33) is still an owner-run acceptance gate.

Phase 0's M4 wrap-up and M5 run in parallel. P1-M1 Task 1 touches only new files, so it can start at once.

---

## Decisions taken

The owner delegated technical decisions (2026-11-10). Each entry gives the choice, why, what was rejected, and the cost if it proves wrong. New dependencies are recorded here and in `THIRD_PARTY_NOTICES.md`. The cargo-deny allowlist in `deny.toml` already covers every licence involved.

**D1. Boolean kernel: `manifold-rust` 0.19.2 (Apache-2.0, pure Rust), its `robust` engine only, behind a `mf-boolean::Kernel` trait. Pinned with `=`. Its use is conditional on the qualification gate in Task 7.**
- **What the engine is.** Exact rational arithmetic (dashu) on a mesh arrangement (Zhou, Grinspun, Zorin and Jacobson 2016, the libigl `mesh_boolean` approach). Every orientation and intersection decision is exact, which is what MF-MODEL-001 asks for. It accepts self-intersecting, non-manifold and doubled-sheet input, and refuses open input with `NotClosed`. It has a cancel token and progress phases, and its `parallel` feature is documented as bit-identical to sequential. Maintainer: Lars Brubaker (MatterHackers, sponsored). It passes the ported Manifold C++ suite and a ~10k-mesh corpus.
- **Rejected:**
  - **A home-grown kernel** (Cherchi et al. 2022 indirect predicates, or our own arrangement). It's the fastest known approach, but 4–6 months of specialist work with the highest risk. It stays the replacement path behind the trait.
  - **C++ Manifold through `manifold3d` or `manifold-csg`.** It needs a C++ toolchain in CI on three OSes and FFI, and its float engine uses symbolic perturbation rather than exact predicates.
  - **`manifold-rust`'s float engine.** It's fast, but it isn't exact-predicate and it needs manifold input. The adapter never calls it.
  - **`boolmesh`.** MPL-2.0 (OD-5 treats file-level copyleft as needing review), and float-based.
  - **`csgrs`.** BSP-based, and not robust on real meshes.
  - **CGAL.** GPL or commercial.
- **Cost if wrong.** The crate is four months old and 0.x, and has one main maintainer. Several things contain that risk:
  - The adapter trait and our own validator mean a bad kernel can at worst cause a refusal, never a silently bad model.
  - Pinning, plus vendoring the source into the cargo cache in CI, protects against upstream churn.
  - If the kernel fails the Task 7 gate or is abandoned, we write ADR-0002b and replace it with a home-grown kernel behind the same trait. That costs months, but no change to the app, the commands or the logs. Meanwhile booleans are labelled as limited, with an issue.

**D2. Exact predicates for our own decisions: `robust` 1.2 (MIT OR Apache-2.0, georust, Shewchuk's adaptive orient2d/orient3d).**
- **Used for:** triangle–triangle intersection, plane-side classification, output validation and the cut-cap orientation tests.
- **Rejected:** `geometry-predicates`, unmaintained since 2021; porting Shewchuk ourselves gains nothing.
- **Cost if wrong:** low. It's a tiny, mature crate with tens of millions of downloads, and `spade` already depends on it.

**D3. Cut-cap triangulation: `spade` 2.15 (MIT OR Apache-2.0), a constrained Delaunay triangulation with exact predicates.**
- **Rejected:**
  - **Ear clipping:** fragile with holes and nested loops.
  - **The `fill_holes` DP:** it's O(n³), capped at 1000 edges, and scan cross-sections run to thousands of edges.
  - **`manifold-rust`'s internal CDT:** not a stable public API.
- **Cost if wrong:** small. Caps go through one function (`triangulate_section`), so the triangulator can be swapped.

**D4. Every modelling result is validated independently, and invalid output is refused by name. There is no automatic retry with another method.**
- **Validation:** `mf_geometry::validate_solid` checks the output is closed, manifold (edges and vertices), consistently oriented, has positive volume, and has no degenerate faces. It also checks that no face created by the operation intersects any other face (exact). Any failure is an `OpError` with a code, a plain-language message and the faces involved, and nothing commits.
- **Rejected:** trusting the kernel, or silently trying a slower method (the owner's no-fallback rule).
- **Cost if wrong:** validation adds time to every operation. It's measured, and it's bounded because only created faces are tested against the BVH.

**D5. Scale by a local window: results cost what the overlap costs, not what the model costs.**
- **How it works.** Before calling the kernel, the target is clipped against an axis-aligned box: the tool's bounds, grown by a margin. Classification against an axis-aligned plane is exact in f64 (a coordinate compared with a constant), and constructed cut points lie exactly on the plane. The clipped part is capped (D3) into a closed mesh and sent to the kernel with the tool. The temporary caps are then removed by provenance and the result is welded back by exact vertex identity.
- **What it keeps.** Faces outside the window are `Kept`, bitwise and in order (MF-OP-003). A kitbash join on a 30M scan therefore touches thousands of faces, keeps selections, and re-uploads only the affected GPU chunks (MF-RENDER-003).
- **Rejected:** whole-mesh kernel calls. A 30M-triangle exact arrangement is neither interactive nor memory-safe.
- **Cost if wrong:** if the window's caps interact badly with the kernel, the validator refuses the result. That puts booleans back to whole-mesh speed, which is labelled as an algorithm limit with an issue.

**D6. A multi-object command contract, extended additively. Spec §4 and §5 are amended in `phase1-spec.md`.**
- Specs gain an `operands:` section.
- `Operation::execute_doc` takes every operand and returns `ObjectChange`s.
- `Sources` gains `operands: Vec<(u32, FaceId)>`, faces of other operands, with `#[serde(default)]`.
- `CommandRecord` gains `operands` and `outputs` (per object: hashes and transform), also with `#[serde(default)]`.
- The log format moves to `mfx-log/2`, and `mfx-log/1` logs still replay. A command's verdict is the worst verdict among its objects.
- **Rejected:**
  - A separate document-op path that bypasses specs: it breaks MF-SPEC-003 and leaves plugins nothing to reuse.
  - Encoding the second object as an integer parameter: it gives no provenance and no validation.
- **Cost if wrong:** a log format bump. Old logs keep reading.

**D7. Transforms are recorded as the absolute resulting matrix. Specs gain the parameter types `vec3` and `affine`.**
- The gizmo and the numeric fields compute the matrix on the UI side: it's tiny arithmetic, not geometry. `object.set_transform` records the 12 f64 values, so replay runs no trigonometry and is Exact on every OS.
- The CLI takes `--param transform=m00,…,m32`, and vectors as `x,y,z`.
- **Rejected:** recording translate, rotate and scale deltas. Platform `libm` `sin`/`cos` differ, and deltas compound.
- **Cost if wrong:** none for replay. Scripting may later want delta helpers, which can be added as more operations.

**D8. Gizmo colours: the conventional X red, Y green, Z blue, on the gizmo only. While the Transform tool is open, the defect and recent-repair tints and outlines are hidden, and the legend shows the axes instead. On screen, a colour still has one meaning.**
- Intersection overlays stay visible, because "move apart" is a transform task. That's why the new `INTERSECTION` token is violet, a hue no other token uses.
- **Rejected:** neutral handles with letter labels. They're unfamiliar to Meshmixer, Fusion and Blender users.
- **Cost if wrong:** if the owner rejects it when reviewing the P1-M1 captures, only the theme tokens change.

**D9. New tokens, all in `theme.rs`, each with one meaning.**

| Token | Value | Meaning |
|---|---|---|
| `INTERSECTION` | violet `#9A6BFF` | faces and curves where objects, or an object and itself, intersect |
| `AXIS_X` / `AXIS_Y` / `AXIS_Z` | `#E5484D` / `#46A758` / `#3E8BF0` | gizmo handles only (D8) |
| `PLATE` | neutral grey 70 | the build-plate grid |
| `CUT_PLANE` | neutral grey 200 at 25% opacity | the split preview plane |
| frozen (sculpt mask) | diagonal hatch in `BACK_FACE` tone | frozen faces; a pattern, not a hue |
| brush cursor | `HOVER` outline | what the pointer is over (an existing meaning) |
| placement preview | `HOVER` glow on the part | what the pointer is over |

**D10. Booleans compute in the target's frame, keep the target's id, name and transform, and consume the tool unless `keep_tool` is set.**
- The tool is brought into the target's frame (the inverse of the target's transform times the tool's), so the target's kept faces are bitwise unchanged. A mirrored tool has its winding flipped.
- Intersection detection uses the same transformed tool, so detection and the boolean always agree.
- **Rejected:** a new result object at identity. It would lose the target's identity, its selections and its incremental GPU upload.

**D11. Split uses our own exact plane cut, plus "split by selected region".**
- **The plane cut** classifies vertices with `orient3d`, splits each crossing edge once (its lower vertex id first, so both faces share the point), and caps each section with D3. On 30M this costs O(n) classification plus local work.
- **Split by selected region** separates a face set and caps both sides with `fill_holes` (smooth). That's Meshmixer's "Separate" plus closure.
- **Drawn-path cuts** (spec MF-MODEL-003) go to Phase 2 as a tracked issue.
- **Rejected:** a split as a boolean with a half-space box. It's whole-mesh cost on a tilted plane, and the window doesn't help a cut that crosses the whole model.
- **Cost if wrong:** if the projected caps self-overlap on near-tangent sections, the cut is refused by name (`cut_section_self_intersects`) and the user is told to move the plane slightly.

**D12 (amended for UC1). Alignment is holes in both cut faces, plus dowels generated as separate objects laid out on the plate. "Attached" pegs and dovetails remain options.**
- **Holes.** A hole of `hole_diameter` is subtracted from each cut face, `hole_depth_above` and `hole_depth_below` deep, through D1 with D5, so the work is local. Facet count is chosen so the chordal error is ≤ `peg_clearance / 10`.
- **Dowels.**
  - Diameter = `hole_diameter − peg_clearance`.
  - Length = `hole_depth_above + hole_depth_below − length_allowance`. The owner's example: holes 5 mm and 8 mm deep give 13 mm before the allowance.
  - Each dowel is a new object, chamfered 0.3 mm at both ends so it enters the hole. It stands upright on the plate (laid out by D21).
- **Defaults** come from the active print fit profile (D20) and are recorded in the parameters. Minimum wall is 1.5 mm. Everything is converted to document units when the command is prepared.
- **Automatic placement.** The pole of inaccessibility of each section region (polylabel, our own code). Two holes go along the region's principal axis where they fit; one keyed hole where only one fits. A region too thin for the hole is refused in plain language, giving the size that would fit.
- **Rejected:**
  - pegs fused to one part as the default: the owner wants loose dowels, and dowels print stronger standing upright;
  - PrusaSlicer-style connectors resolved at slice time: ours are geometry, so the clearance is measured on the meshes.
- **Cost if wrong:** low. `pegs: attached` (the previous design) stays available as a parameter value.

**D13. Sculpting comes before mixing.**
- Meshmix's blend needs local remeshing (MF-SCULPT-002) and region fairing (from MF-OP-002). Sculpting builds both first.

**D14. Mixing joins by boolean union plus an optional blend. Meshmixer-style zippering of an open boundary is not built in Phase 1.**
- **The blend:** a seam band within `blend_width` is remeshed (D13) and then faired, Δ² with two fixed rings outside it, and validated (D4).
- **Open "stamp" parts** are capped automatically (`fill_holes` flat) and sunk by `embed_depth` along the attach normal before the union, so the blend hides the cap. One robust path serves both closed and open parts.
- **Rejected: zippering.** It needs projecting a curve onto the surface, cutting the target along it and bridging, which is a second fragile kernel with no manifold guarantee.
- **Cost if wrong:** if blends on thin, open stamp parts look worse than Meshmixer's, zippering is a Phase 2 issue (registered in Task 34).

**D15 (amended 2026-11-10 for mixing and UC2). Open targets: joins use local-window closure; subtract and intersect need a closed target.**
- **Subtract and intersect** (P1-M2) are refused on an open target, because "inside" isn't defined for it. The refusal comes with a one-click "Fill all holes, then combine": `fill_holes` and then the operation, as two undoable commands.
- **Unions are local** (joins, mixing, connector leaves):
  - Holes outside the D5 window don't matter, because those faces are kept as they are.
  - A hole whose loop lies wholly inside the window is closed temporarily for the kernel (`fill_holes` flat, faces tagged as temporary). An example is UC2's neck after the user deletes the cut cap.
  - After the union, a temporary face must not survive, since the tool must cover the opening. If one survives, the join is refused as `opening_not_covered` ("The head doesn't cover the opening at the neck; move it to cover the opening, or fill the hole first"). An unrequested fill is never left behind.
  - A hole that crosses the window's boundary grows the window to contain its loop, up to 4× the tool's bounds. Beyond that, the join is refused as `opening_too_large_for_join`, with the same fill-first next action.
  - The result is welded back into the open scan by exact vertex identity, as in D5. Its other holes stay open and stay listed.
- **Rejected:**
  - refusing every open target (the first version of D15), which fails UC2 and Meshmixer parity on scans;
  - filling the user's holes silently.
- **Cost if wrong:** if temporary closure upsets the kernel near thin openings, those joins are refused by name, and the user fills first. The refusal's frequency on the messy corpus is reported, and kept as an issue if it's above 5%.

**D16. The parts library is a manifest per part, with built-ins generated in code and a user folder.**
- Each part has `part.yaml` (id, name, version, licence, source, `kind: solid|open`, attach point, normal and up, default scale) next to its mesh. Built-ins are procedural (sphere, cylinder, cone, box, rounded box, torus, capsule, hemisphere stamp), so no third-party asset licences are needed.
- User parts live in the platform config directory. "Save object as part" writes one.
- Plugins (MF-PLUG-001) will add parts through the same manifest.
- Part sources are content-hashed, like imports, so replay verifies them.

**D17. The face BVH moves from `mf-render` to `mf-geometry`, and `mf-render` re-exports it unchanged.**
- Intersection detection, validation, the window, sculpting and picking all need it, and `mf-operations` must not depend on `mf-render`. `mf-geometry` gains `glam` (already a workspace dependency).

**D18. New crates and dependency direction (amends spec §2):**
- `mf-boolean` (depends on geometry, runtime, `manifold-rust`, `robust`, `spade`), which holds the kernel adapter, the window, plane clipping and section triangulation;
- `mf-parts` (depends on geometry, io, spec, `mf-boolean` for primitives), which holds the manifest, built-in generators, the user library, the config-directory adapter (D20) and the `Connector` trait with the built-in connectors (D22).
- `mf-operations` depends on both. Frontends stay leaves.

**D19. Pen pressure comes from egui touch `force` where the platform reports it. Elsewhere pressure is 1.0, and the brush panel says "Pen pressure not reported by this device".**

**D20. A print fit profile holds every printing tolerance. Operations read their defaults from it and record the values they used.**
- **Fields and built-in defaults** ("Default FDM, 0.4 mm nozzle"):

  | Field | Default | Meaning |
  |---|---|---|
  | `peg_clearance` | 0.2 mm | dowel diameter is the hole diameter minus this |
  | `length_allowance` | 0.4 mm | the dowel is shorter than the two holes together by this |
  | `hinge_gap` | 0.4 mm | gap around moving parts printed in place, and the lid's cut gap |
  | `clasp_interference` | 0.3 mm | how far a clasp's lip passes under its catch |

- **Where it lives.** Profiles are saved to `<config dir>/meshfixer/fit-profiles.yaml`. The built-in profile is read-only; users duplicate and edit it in **Settings ▸ Print fit**, and pick the active one there. The CLI takes `--fit-profile <name>`.
- **How operations use it.** A spec parameter may declare `default_from: fit.<field>`. The frontend fills it from the active profile at prepare time, so the recorded parameters hold the actual values and replay never reads the profile. CLI help and MCP schemas show the built-in value as the default.
- **The config directory** is found by a small marked adapter module (`mf-parts::dirs`), following MF-PLAT-001, with no new dependency. D16's user parts library uses it too.
- **Rejected:**
  - constants baked into each spec: the owner wants one reusable place;
  - reading the profile at execute time: replay would depend on the machine's settings.
- **Cost if wrong:** low. The profile is a frontend-side defaults source, and the operations never see it.

**D21. Generated loose parts are laid out on the plate by one shared, deterministic function.**
- `layout_on_plate(occupied: &[Footprint], new: &[Footprint], plate, spacing = 3 mm) -> Vec<[f64; 12]>` packs rows into free plate space, starting from the corner nearest the origin and avoiding every existing object's world footprint.
- Dowels (UC1) use it, and so does `object.arrange_on_plate`. Its order is fixed by input order, so replay is Exact.
- **Rejected:** placing dowels next to their holes. They would overlap the parts on the plate.
- **Cost if wrong:** low. A layout that doesn't fit the plate is reported ("3 dowels placed off the plate; the plate is full"), never silently stacked.

**D22. Parametric connectors (MF-MODEL-005) are brought into this plan as P1-M6, behind a `Connector` trait that plugins can implement.**
- **The trait.** A connector is code plus a spec: `fn generate(&self, params, ctx) -> ConnectorParts`, where each part has a role (`leaf_a`, `leaf_b`, `pin`, `hook`, `catch`), a mesh, which side it joins (A, B or none), and how it joins (union, subtract, or stay separate).
- **One object per placed connector.** Its parts are components of one mesh, and `Object.connector: Option<ConnectorInstance>` records the connector's id, version, parameters and each role's face range. The gizmo moves the whole connector, so a hinge can't be pulled apart.
- **Resize is parametric.** For connectors, the gizmo's scale handles are replaced by **Size** fields, which regenerate the mesh (`object.edit_connector`). Scaling the mesh would scale the hinge gap and the clasp interference too.
- **Joining.** `connector.join` unions each role into its side's object in one command, through the same `mesh.boolean` code (D15 local unions). It then checks that the moving parts stay separate, by detecting intersections and measuring the gaps.
- **Plugins.** MF-PLUG-001 plugins will implement the same trait over the versioned plugin interface. Built-in connectors are registered exactly as plugin ones will be.
- **Rejected:**
  - connectors as fixed mesh parts: they can't be resized without breaking their gaps;
  - each leaf as its own object: they could be moved apart.
- **Cost if wrong:** the `Object.connector` field is additive. If 3MF export drops the metadata, connectors become plain meshes, which is fine once merged. An unmerged connector re-imports as a plain mesh, and that limit is recorded as an issue.

**D23. `mesh.split_plane` gains `cut_gap`, which removes a slab of that thickness at the cut.**
- Print-in-place parts (UC3's lid on its body) need it, otherwise the lid and body would print fused. The default is 0. The connector tool proposes `hinge_gap` for it.
- **Rejected:** moving the lid up afterwards. The hinge axis would no longer sit on the seam.

**D24. Scenario fixtures are procedural and deterministic.**
- `mf-fixtures` gains `octopus()`, `head()` and `hollow_box(wall)`.
- An optional CC0 octopus and head, fetched by hash and recorded in `fixtures/SOURCES.md`, make the GUI captures look real. The scenario tests never depend on them.
- **Cost if wrong:** none for the gates. The captures just look plainer.

---

## Global Constraints

- **Unchanged from M4:**
  - MIT/Apache licences and Rust 1.99.1.
  - Deterministic results at any thread count (MF-CMD-005 strict).
  - No silent fallbacks: reviewed defaults carry `// allow-swallow: <reason>`.
  - Commit with `cargo fmt --all && scripts/check.sh && git commit`, never `;`.
  - Never switch branches with uncommitted changes. Every delete uses an absolute path.
- **No geometry on the UI thread** (MF-RT-001): no boolean, cut, validation, BVH build, intersection test or stroke solve. No UI frame exceeds **50 ms** while any Phase 1 operation runs on 5M triangles.
- **Cancellation:** long work checks its token at least every **100 ms**. Cancel-to-stop is **≤ 250 ms**, including inside the kernel (Task 7 bridge). A cancelled run changes no document state (MF-CMD-006).
- **Progress** is monotonic in 0–1, with stage words, in the GUI and with `--progress`.
- **Interactive targets:**
  - 1M triangles ≥ 60 fps, 5M ≥ 30 fps, 10M ≥ 15 fps (MF-RENDER-002).
  - The owner's binding goal: **interactive at 5–30M**. Every milestone measures 30M too. M2 Max results are preliminary (OD-2).
- **Memory:** ≤ 400 MB of working geometry plus topology per 5M (MF-GEO-002). Kernel and stroke scratch are reported separately as peak heap, under the M4 breakdown.
- **Every operation:**
  - is versioned and spec-driven (MF-SPEC-001…003);
  - is targetable where that makes sense;
  - records total provenance (MF-SEL-002, extended by D6);
  - is undoable;
  - replays **Exact** on the same build and **Equivalent** across OSes.
- **Never inflate results.** An invalid output is refused by name (D4). An open, non-manifold or self-intersecting input is either handled or refused by name, with an obvious next action.
- **Printing parameters are in document units** (spec §11b). Defaults are written in mm in the spec and converted at prepare time. The converted value is what's recorded.
- **Tolerances come from the active print fit profile (D20).** The values used are recorded in each operation's parameters, and replay never reads the profile.
- **Use cases are gates.** UC1, UC2 and UC3 each pass as an automated scenario (commands or CLI) plus a GUI frame capture before their milestone closes.
- **Checkpoints never block.** Captures go to `artifacts/phase1/<milestone>/`, and work continues.
- **Plain language and one colour, one meaning** (`docs/design/visual-language.md`, D8, D9). Every refusal has a title, a detail and a next action in the copy table.
- **Limits are labelled for what causes them.** An algorithm limit says so and cites its issue ID.
- **Heavy and timing tests run on demand only.** 5M/30M and benchmark tests go in the `timing=(…)` list in `scripts/run-tests.sh` and `heavy.yml`. Large fixtures are generated into `mktemp -d` and deleted afterwards.
- **Do not edit M4's in-flight files** (`app.rs`, `panels.rs`) until M4 is merged. Tasks that need them say so and start after the merge.

## Review Focus

1. **Touching and coplanar operands.** Two cubes sharing a face or an edge, or a part resting exactly on a flat surface. A union can create an edge with four faces. The result must be valid or refused by name (`result_not_manifold`, "The objects only touch along an edge; move one slightly so they overlap"). Test `touching_operands_are_valid_or_refused_by_name` (Task 8).
2. **Mirrored and sheared transforms from imported 3MF.** A mirrored tool must keep outward normals in the result. A sheared transform must not be decomposed into fake rotate and scale fields: those fields say "Transform includes shear (from the file)" and stay read-only, while the gizmo's move handles still work. Tests `boolean_with_mirrored_tool_keeps_outward_normals` (Task 8), `sheared_transform_disables_rotate_and_scale_fields` (Task 5).
3. **Cancel at any stage, including inside the kernel.** Window clip, kernel, weld, validate, blend, or mid-stroke. The document hash must be unchanged and there must be no revision. Test `cancel_at_every_stage_leaves_document_unchanged` (Task 8; also in Tasks 12 and 19).
4. **Unit-scaled documents.** In a metre-scale scan (MF-ISSUE-045), the default 0.2 mm clearance must be 0.0002 document units, and that's what is recorded. Test `default_clearance_is_0_2_mm_in_a_metre_document` (Task 13).
5. **Undo and replay of multi-object commands.** Undoing a boolean that consumed its tool restores the tool with the same `ObjectId`. A selection on the tool translates to `Failed(ObjectMissing)` afterwards. Replaying the log reproduces both objects with Exact verdicts, and a 3MF export after the undo contains both. Test `undo_boolean_restores_the_tool_with_its_id` (Task 3).

---

# P1-M1: Object transform gizmo (MF-OBJ-001)

**Meshmixer benchmark:** Edit ▸ Transform (move, rotate and scale handles, numeric fields) and Edit ▸ Align or drop to the plate.

**Exceed:**
- dragging a 30M-triangle object is as smooth as orbiting it, because only a uniform changes;
- every move is an undoable command that replays Exact on every OS (D7);
- f64 numeric precision;
- snapping to grid, angle and plate;
- transforms round-trip through 3MF;
- mirrored and sheared transforms from files are handled honestly (Review Focus 2).

### Task 1: Gizmo geometry, hit-testing and drag maths (start here)

**Files:**
- Create: `crates/mf-render/src/gizmo.rs`
- Modify: `crates/mf-render/src/lib.rs` (export)
- Test: `crates/mf-render/tests/gizmo.rs`

**Interfaces:**
- **Consumes:** `OrbitCamera` (`ray`, `view_proj_f64`), `model_matrix`, all existing.
- **Produces** (pure f64, no GPU, no egui):
  - `enum GizmoMode { Move, Rotate, Scale }`.
  - `enum Handle { Axis(u8), Plane(u8), Ring(u8), ViewRing, ScaleAxis(u8), ScaleUniform, Centre }`. The axis index runs 0..3 in world X/Y/Z.
  - `struct Gizmo { pivot: DVec3, mode: GizmoMode, size_pt: f64 }`, with `size_pt` defaulting to 110.
  - `Gizmo::handles(&self, cam: &OrbitCamera, view: (f64, f64)) -> Vec<HandleShape>`: screen-space shapes the renderer draws, in a constant size on screen.
  - `Gizmo::hit(&self, cam, view, px: (f64, f64)) -> Option<Handle>`: nearest within 8 pt. Axis handles beat plane handles, which beat rings, which beat the centre. An axis seen within 10° of end-on isn't offered.
  - `struct Drag`, created by `Gizmo::begin(handle, start_px, cam, view, start: [f64; 12])`, with `Drag::update(&self, px, snap: &Snap) -> [f64; 12]`, which returns the absolute transform.
  - `struct Snap { grid: Option<f64>, angle_deg: Option<f64>, scale_step: Option<f64>, plate: Option<PlateSnap> }`, where `PlateSnap { world_min_z: f64, within_pt: f64 }`. Defaults are a 1 mm grid, 15°, steps of 0.1, and 8 pt from the plate.
  - `pub fn decompose(t: &[f64; 12]) -> Decomposed`, with `enum Decomposed { Trs { translate, rotate_deg_xyz, scale }, Sheared }`. The rotation is extrinsic XYZ. Mirroring is reported as a negative X scale.
  - `pub fn compose(translate: DVec3, rotate_deg_xyz: DVec3, scale: DVec3, pivot: DVec3) -> [f64; 12]`.

- [ ] **Step 1: Write the failing tests**
  - `axis_drag_moves_only_along_that_axis`: drag the X arrow 100 px. Only `t[9]` changes, by the distance the ray–axis closest point moved (to 1e-9).
  - `plane_handle_moves_in_its_plane`
  - `ring_drag_rotates_about_pivot_and_snaps_to_15_degrees`: a 37° drag gives exactly 30° with snapping on and 37° ± 0.01 with it off. The pivot stays fixed (to 1e-12).
  - `uniform_scale_about_pivot`
  - `grid_snap_rounds_translation_to_1mm`
  - `plate_snap_lands_min_z_on_zero_within_8pt`: snaps when the screen distance is ≤ 8 pt, not at 9.
  - `end_on_axis_is_not_hittable`
  - `handles_keep_screen_size_at_any_zoom`: the projected arrow length is 110 ± 1 pt at zoom ×0.01 and ×100.
  - `decompose_compose_round_trip` (rotations at 0°, 90°, 179.9° and gimbal-adjacent angles), `mirrored_transform_decomposes_with_negative_x_scale`, `shear_is_reported_as_sheared`.
- [ ] **Step 2:** Run `cargo test -p mf-render --test gizmo`. Expected: FAIL, as the module doesn't exist yet.
- [ ] **Step 3:** Implement `gizmo.rs`.
  - An axis drag uses the closest point between the pointer ray and the axis line.
  - A ring drag uses the signed angle of the ray's hit on the ring's plane, around the pivot.
  - The rotation is applied as `T(pivot)·R·T(−pivot)·start`, composed in the 3MF row-vector convention that `model_matrix` documents.
- [ ] **Step 4:** Run `cargo test -p mf-render --test gizmo`. Expected: PASS.
- [ ] **Step 5:** Commit `feat(render): gizmo handles, hit-testing and drag maths [MF-OBJ-001]`.

### Task 2: Phase 1 spec, ADRs and the operand contract in specs

**Files:**
- Create: `docs/spec/phase1-spec.md`, `docs/adr/0002-boolean-kernel.md`, `docs/adr/0003-multi-object-commands.md`, `docs/adr/0004-mixing-join.md`
- Modify: `specs/operation.schema.json`, `crates/mf-spec/src/{model.rs, params.rs, cli.rs}`, `THIRD_PARTY_NOTICES.md` (written as each dependency is added)
- Test: `crates/mf-spec/tests/` (the schema and parity tests that exist)

**Interfaces:**
- **Produces:**
  - Spec YAML gains an optional `operands:` list: `{ name, role: read|consumed|kept, kinds: [object, face_set, component], required: bool }`.
  - `geometry_kinds` and the target stay as they are.
  - `ParamKind::Vec3 { default: [f64; 3], unit }` and `ParamKind::Affine { default: [f64; 12] }`.
  - `ParamValue::Vec3([f64; 3])` and `ParamValue::Affine([f64; 12])`. Serialized as JSON arrays, and in the CLI as comma-separated values.
  - `ParamSpec.unit: Some("mm")` marks a length. `OperationSpec::to_document_units(&mut Params, unit: Unit)` converts it, and runs in `prepare`.
  - `OperationSpec.operands: Vec<OperandSpec>`.
  - `phase1-spec.md` numbers D1–D19 as testable requirements: MF-P1-CMD-001 (operands and object changes), MF-P1-SEL-001 (operand provenance), MF-P1-VAL-001 (D4), MF-P1-SCALE-001 (D5), the performance targets of each milestone below, and UC1–UC3 as acceptance requirements (MF-P1-UC-001…003), each with its scenario assertions copied from Tasks 16a, 24a and 30.
- [ ] **Step 1:** Tests:
  - `vec3_and_affine_params_validate_and_round_trip`;
  - `operands_section_validates_against_schema`;
  - `lengths_convert_to_document_units`;
  - the existing parity test still passes.
- [ ] **Step 2–4:** FAIL → implement → PASS (`cargo test -p mf-spec`).
- [ ] **Step 5:** Commit `feat(spec): operands, vec3/affine params, unit conversion; Phase 1 spec and ADRs`.

### Task 3: Object changes through the command layer, `object.set_transform` and `object.drop_to_plate`

**Files:**
- Modify:
  - `crates/mf-operations/src/lib.rs`
  - `crates/mf-geometry/src/ids.rs` (`Sources.operands`)
  - `crates/mf-document/src/lib.rs` (`with_transform`, `without_object`, `with_new_object`)
  - `crates/mf-command/src/{session.rs, record.rs, log.rs, replay.rs}`
  - `crates/mf-cli/src/main.rs` (operand args)
- Create:
  - `crates/mf-operations/src/transform.rs`
  - `specs/operations/object.set_transform.yaml`
  - `specs/operations/object.drop_to_plate.yaml`
- Test: `crates/mf-command/tests/objects.rs`, `tests/geometry/transform.yaml`

**Interfaces:**
- **Consumes:** Task 2's types.
- **Produces:**
  - `struct OperandInput<'a> { object: ObjectId, mesh: &'a Mesh, transform: [f64; 12], target: Target }`. Operand 0 is the command's target.
  - `enum ObjectChange { Geometry { operand: usize, mesh: Mesh, provenance: Provenance }, Transform { operand: usize, transform: [f64; 12] }, Remove { operand: usize }, Create { name: String, mesh: Mesh, transform: [f64; 12], provenance: Provenance } }`. A created object's provenance lists every face as `Created`, with sources in `Sources.operands`.
  - `struct DocOutput { changes: Vec<ObjectChange>, report: serde_json::Value }`.
  - `Operation::execute_doc(&self, inputs: &[OperandInput], params: &Params, ctx: &RunContext) -> Result<DocOutput, OpError>`. The default implementation wraps `execute` on operand 0, so existing operations don't change.
  - `Invocation.operands: Vec<(String, ObjectId, Option<Selection>)>`.
  - `CommandRecord.operands: Vec<RecordedOperand { name, object, selection, input_revision_hash, input_structure_hash, input_transform }>`, `CommandRecord.outputs: Vec<ObjectOutput { object, created: bool, removed: bool, hash, structure_hash, transform }>`. Both have `#[serde(default)]`.
  - `LOG_FORMAT = "mfx-log/2"`. Replay accepts `mfx-log/1`.
  - The verdict for a transform-only change is Exact if the transform bits are equal, and Mismatch otherwise.
  - Commit is O(changed objects): an unchanged geometry reuses its cached hash.
  - Operations:
    - `object.set_transform` v1: param `transform: affine`, whole object only.
    - `object.drop_to_plate` v1: no params. Computes the world-space minimum z of the transformed vertices, cancellably on the worker, and translates by its negation.
    - `object.delete` v1: removes the target object (`ObjectChange::Remove`). Undo restores it with the same id. UC2 uses it to delete the head piece. In the desktop it's the Delete key on an object target.
  - CLI:
    - operands as `--operand tool=<object index in the opened file>`;
    - `meshfixer run in.3mf --op object.set_transform --param transform=…`.
- [ ] **Step 1:** Tests:
  - `set_transform_is_one_undoable_command_and_replays_exact`;
  - `drop_to_plate_puts_world_min_z_at_zero` (rotated 30° about X);
  - `delete_object_is_undoable_and_keeps_its_id`;
  - `transform_change_keeps_geometry_shared` (`Geometry::ptr_eq`);
  - `undo_boolean_restores_the_tool_with_its_id` (Review Focus 5, using a test-only operation under `cfg(feature = "test-kinds")` that removes operand 1 and creates an object);
  - `mfx_log_1_still_replays` (the committed M2/M3 logs);
  - `operand_provenance_is_total` (property test: every output face of every changed or created object is accounted for exactly once, and every face of a consumed operand is a source or removed);
  - `edited_transforms_round_trip_through_3mf`.
- [ ] **Step 2–4:** FAIL → implement → PASS (`cargo test -p mf-command -p mf-operations -p mf-cli`).
- [ ] **Step 5:** Commit `feat(command): operands and object changes; set_transform, drop_to_plate [MF-OBJ-001]`.

### Task 4: Draw the gizmo and the build plate

**Files:** `crates/mf-render/src/{renderer.rs, shader.wgsl}`, `crates/mf-render/src/overlay.rs` (new). Test: `crates/mf-render/tests/gizmo_render.rs`

**Interfaces:**
- **Consumes:** `Gizmo::handles` from Task 1.
- **Produces:**
  - `MeshRenderer::draw_gizmo(&mut self, pass, shapes: &[HandleShape], highlight: Option<Handle>, palette: &GizmoPalette)`. It runs after every other layer, with no depth test, as screen-space quads like the existing 3 pt lines. The hovered handle is drawn 40% lighter and twice as wide, as the selection pulse is.
  - `MeshRenderer::draw_plate(&mut self, pass, plate: &Plate)`, with `Plate { size_mm: [f64; 2], grid_mm: f64 }`. The default is 256 × 256 with a 10 mm grid, at world z = 0, in `PLATE`, depth-tested, and fading with distance.
  - Colours come from `theme.rs` (D9) through `GizmoPalette`. The renderer has no colours of its own.
- [ ] **Step 1:** Offscreen tests:
  - `gizmo_draws_over_geometry`;
  - `plate_is_hidden_behind_objects_resting_on_it`;
  - `highlighted_handle_is_lighter`.
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(render): gizmo and build plate overlays [MF-OBJ-001]`.

### Task 5: The Transform tool in the desktop (after M4 merges)

**Files:**
- Modify: `crates/mf-desktop/src/{app.rs, panels.rs, viewport.rs, theme.rs}`, `docs/design/visual-language.md`
- Create: `crates/mf-desktop/src/transform_tool.rs`
- Test: `crates/mf-desktop/tests/transform.rs`

**Behaviour:**
- **Opening it.** An object's tree row, or clicking an object while no defect is targeted, makes the object the target. Properties then lists **Transform** first among the whole-object tools. `T` opens it, with `W`, `E` and `R` for Move, Rotate and Scale, as in Meshmixer.
- **Dragging.**
  - A drag changes only that object's model uniform. Releasing it commits one `object.set_transform`: one undo per drag.
  - Esc during a drag restores the start transform and commits nothing.
  - Holding Cmd/Ctrl while dragging inverts snapping.
- **Numeric fields.** Position (bounding-box centre, in document units), Rotation (degrees) and Scale (%, with a uniform lock). Enter commits.
  - A sheared transform shows the "Transform includes shear (from the file)" note (Review Focus 2).
  - A mirrored object shows "Mirrored".
- **Buttons:** **Drop to plate** (runs on the worker, with progress if it's slow) and **Reset**.
- **Overlays.** While the tool is open, defect and recent-repair overlays are hidden and the legend shows the axes (D8). An intersection overlay, once built (P1-M2), stays.
- **Status line.** "Moved scan by 12.0, 0, 0 mm", "Rotated plate 90° about Z".

- [ ] **Step 1:** GUI tests:
  - `drag_x_arrow_moves_object_and_one_undo_restores_it`;
  - `escape_mid_drag_commits_nothing`;
  - `numeric_rotation_commits_one_command`;
  - `sheared_transform_disables_rotate_and_scale_fields`;
  - `transform_tool_hides_defect_tints_and_shows_axes_in_legend`;
  - `drop_to_plate_button_lands_on_plate`.
  - Timing (heavy list): `dragging_a_30m_object_keeps_frames_under_orbit_plus_1ms` and `ui_frames_stay_under_50ms_while_dropping_30m_to_plate`.
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(desktop): transform tool with gizmo, snapping and drop to plate [MF-OBJ-001]`.

### Task 6: Checkpoint P1-A (capture and continue)

- [ ] Capture from the running app at 1440×900 (PNGs plus a 20–40 s MP4, `scripts/capture-checkpoint-p1a.sh`, after the M4 capture scripts):
  - two objects;
  - move, rotate and scale with snapping;
  - drop to plate;
  - undo;
  - the numeric fields;
  - a 30M object dragged, with the frame-time overlay.
- [ ] Put them in `artifacts/phase1/p1-m1/`, along with `scripts/demo-p1a.sh`, list them in `docs/milestones/p1-m1.md`, and continue. The owner reviews every checkpoint together later.

**Exit criteria (P1-M1):**
- MF-OBJ-001 is met. Transforms are commands, Exact on replay, and stored in 3MF.
- Drag frame time at 5M, 10M and 30M is within orbit frame time plus 1 ms (p95).
- A transform commit takes < 1 ms on the UI thread.
- CI is green on three OSes.
- The checkpoint captures are in `artifacts/phase1/p1-m1/` (the owner reviews them later, without blocking).

---

# P1-M2: Intersection analysis and booleans (MF-MODEL-002, MF-MODEL-001)

**Meshmixer benchmark:** Edit ▸ Boolean Union, Difference and Intersect. Meshmixer's booleans are approximate and often fail, or need Inspector clean-up afterwards. Meshmixer has no view of where objects intersect.

**Exceed:**
- exact predicates for every decision;
- never an invalid result (D4); a refusal names the reason and gives the next action;
- intersections shown before you act, with Union, Subtract and Move apart offered;
- provenance says which input every face came from;
- local cost on 30M targets (D5);
- cancel and progress;
- Exact replay.

### Task 7: Kernel qualification spike (go/no-go for D1)

**Files:**
- Create: `crates/mf-boolean/{Cargo.toml, src/lib.rs, src/kernel.rs, src/manifold.rs}`, `crates/mf-boolean/tests/qualify.rs`
- Modify: workspace `Cargo.toml` (`manifold-rust = { version = "=0.19.2", default-features = false, features = ["parallel"] }`), `deny.toml` (no change expected; confirm)

**Interfaces:**
- **Produces:**
  - `pub enum BoolOp { Union, Subtract, Intersect }`.
  - `pub trait Kernel: Send + Sync { fn boolean(&self, a: &KernelMesh, b: &KernelMesh, op: BoolOp, ctx: &RunContext) -> Result<KernelResult, KernelError>; fn id(&self) -> &'static str; }`.
  - `struct KernelMesh { positions: Vec<[f64; 3]>, triangles: Vec<[u32; 3]>, face_ids: Vec<(u8 /* operand */, u32 /* face */)> }`.
  - `struct KernelResult { positions, triangles, origin: Vec<(u8, u32)> }`: the input face each output triangle lies on, from Manifold's `TriRef`.
  - `enum KernelError { NotClosed { operand: u8 }, Cancelled, Internal(String) }`.
  - `ManifoldRobust` implements `Kernel` through `robust::boolean_with_progress` with `MeshGL64`.
  - `fn bridge(ctx: &RunContext) -> (manifold_rust::cancel::CancelToken, BridgeGuard)`. A scoped watcher forwards our cancel within 10 ms, and progress phases map onto a `Span`.

**Gate (all must pass; results go to `artifacts/phase1/p1-m2/qualify.json` with device info):**
1. **Corpus:** every pair from the M4 messy collection (each model unioned with a sphere through its centroid; with itself translated 10%; and subtracting a box) gives either a result that passes `validate_solid` or a named `KernelError`. There are 0 invalid outputs and 0 panics (caught with `catch_unwind` in the test only).
2. **Determinism:** identical output hashes at 1 and N threads, and across two runs.
3. **Provenance:** every output triangle's `origin` names a real input face. Output triangles lying on one input face cover its area within 1e-9.
4. **Cancel-to-stop ≤ 250 ms** on the 1M × 1M case.
5. **Time and peak heap:** 1M × 1M overlapping union ≤ 10 s, and 50k × 50k ≤ 0.5 s, on the M2 Max.

- [ ] **Step 1:** Write `qualify.rs` with the five gates (heavy list: `qualify_corpus`, `qualify_1m_union`, `qualify_cancel`).
- [ ] **Step 2:** Run it: `scripts/run-tests.sh` on demand, or with the heavy workflow.
- [ ] **Step 3: Decision record.** If every gate passes, set ADR-0002 to Accepted. If any fails, ADR-0002 records which one, and the work stops for a replacement-kernel plan (the home-grown path of D1). Booleans aren't shipped on a failing kernel.
- [ ] **Step 4:** Commit `feat(boolean): kernel adapter and qualification [MF-MODEL-001]`.

### Task 8: `mesh.boolean`: the window, the weld and the validator

**Files:**
- Create:
  - `crates/mf-boolean/src/{window.rs, clip.rs, section.rs, weld.rs}`
  - `crates/mf-geometry/src/validate.rs`
  - `crates/mf-operations/src/boolean.rs`
  - `specs/operations/mesh.boolean.yaml`
- Move: `crates/mf-render/src/bvh.rs` → `crates/mf-geometry/src/bvh.rs` (D17; `mf-render` re-exports it, and the pick tests stay unchanged)
- Test: `crates/mf-operations/tests/boolean.rs`, `tests/geometry/boolean.yaml`

**Interfaces:**
- **Consumes:** the `Kernel` trait from Task 7; `ObjectChange` from Task 3.
- **Produces:**
  - `mf_geometry::validate_solid(mesh: &Mesh, created: &[FaceId], bvh: &Bvh, ctx) -> Result<Result<(), SolidDefect>, Cancelled>`, with `enum SolidDefect { Open { edges }, NonManifoldEdge { edges }, NonManifoldVertex { vertices }, Inconsistent { edges }, Degenerate { faces }, SelfIntersects { pairs }, NonPositiveVolume }`.
  - `mf_boolean::clip_to_box(mesh, aabb, ctx) -> Clipped { inside: Mesh, inside_faces: Vec<FaceId>, seam: Vec<VertexId>, outside_kept: Vec<FaceId> }`. The inside part is closed by caps whose faces are tagged as temporary. For a union, any hole whose loop lies inside the window (growing the window to contain loops that cross it, up to 4× the tool's bounds) is also closed by temporary `fill_holes` flat faces (D15). Every temporary face must be gone after the kernel, or the join is refused.
  - `mf_boolean::triangulate_section(loops: &[Vec<[f64; 2]>]) -> Result<Vec<[u32; 3]>, SectionError>`, using spade CDT with nesting by orientation. A self-intersecting loop gives `SectionError::SelfIntersects`.
  - `mf_boolean::boolean(target: &Mesh, tool_in_target_frame: &Mesh, op: BoolOp, kernel: &dyn Kernel, ctx) -> Result<BooleanOutput, BooleanError>`, with `BooleanOutput { mesh: Mesh, provenance: Provenance, window_faces: usize }`. The output is built with `MeshEdit` on the target: outside faces are Kept, and created faces have `Sources.faces` (target) or `Sources.operands` (tool, operand 1).
  - `mf_boolean::operand_in_frame(tool: &Mesh, tool_t: &[f64; 12], target_t: &[f64; 12]) -> Mesh`. It flips winding when the determinant is negative. Intersection detection uses it too (D10).
  - `mesh.boolean` v1:
    - operand `tool` (`consumed`, kinds: object or component);
    - params: `operation: choice [union, subtract, intersect]` (default union), `keep_tool: bool` (false), `blend_width: float mm` (0, used by P1-M5).
    - Its stages are "finding the overlap", "cutting", "combining", "joining back" and "checking the result".
  - **Refusals** (`OpError::Failed` codes, each in the copy table):

    | Code | Plain language | Next action |
    |---|---|---|
    | `operand_not_closed` | subtract or intersect on an open target: "Scan has 12 holes; cutting needs a closed model" | "Fill all holes, then combine" (D15) |
    | `tool_not_closed` | the tool is open and isn't an open library part (those are capped by Task 22) | "Fill the tool's holes first" |
    | `opening_not_covered` | a temporary closure face survived a union (D15) | "Move the part to cover the opening, or fill the hole first" |
    | `opening_too_large_for_join` | an opening crosses the window by more than 4× the tool's bounds (D15) | "Fill the hole first" |
    | `no_overlap` | "These objects don't touch" | "Move one onto the other" |
    | `result_not_manifold` | — | Review Focus 1's copy |
    | `result_self_intersects` | — | — |
    | `section_self_intersects` | — | — |
    | `kernel_internal` | names the kernel and version | — |

- [ ] **Step 1:** Tests:
  - `union_subtract_intersect_of_offset_cubes_match_exact_volumes` (volumes exact to 1e-12 relative);
  - `faces_outside_the_window_are_kept_bitwise` (`untouched_outside_target`);
  - `touching_operands_are_valid_or_refused_by_name` (Review Focus 1);
  - `boolean_with_mirrored_tool_keeps_outward_normals` (Review Focus 2);
  - `subtract_on_open_target_is_refused_with_fill_holes_next_action`;
  - `union_onto_open_target_closes_a_covered_opening_locally` (a sphere with its cap deleted, and a larger sphere covering the opening: the result is closed and manifold, and holes elsewhere stay open and unchanged);
  - `union_with_uncovered_opening_is_refused_as_opening_not_covered`;
  - `opening_far_outside_the_window_doesnt_block_the_union`;
  - `cancel_at_every_stage_leaves_document_unchanged` (Review Focus 3; the instrumented context cancels at each stage name);
  - `boolean_replays_exact_and_strict_at_1_and_n_threads`;
  - `consumed_tool_is_removed_and_its_faces_are_sources`;
  - `validator_catches_an_injected_self_intersection` (a fault-injected kernel returns a bad mesh, and the result is refused);
  - `messy_corpus_booleans_are_valid_or_named` (heavy).
  - Timing (heavy):
    - `union_30m_scan_with_50k_part_under_3s`;
    - `union_1m_by_1m_under_10s`;
    - `cancel_within_250ms_in_kernel`.
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(boolean): mesh.boolean with local window and independent validation [MF-MODEL-001]`.

**Controller amendment (2026-11-10, after Task 7b; ADR-0002 option B):**
- **Kernel:**
  - The kernel is `ManifoldCpp`, with the operand check.
  - The `manifold-cpp` feature becomes the default for `mf-boolean`, and `mf-manifold-sys` joins the workspace. It is verified locally on macOS only (owner ruling 2026-11-11: no GitHub Actions). The Linux and Windows builds are an explicit, documented risk in ADR-0002 and the milestone report. Update the `run-qualify` label references in ADR-0002 and run-tests.sh to "manual dispatch only".
  - `ManifoldRobust` stays behind its own off-by-default feature, for comparison only.
- **Operand check:**
  - The self-intersection check runs on the window operands (`clip_to_box` output), never on the whole model.
  - Coincident vertices are welded exactly before the check.
  - Clipping a model whose crossings lie outside the window must not report them.
- **New refusal code** for the copy table: `operand_overlaps_itself`. Plain language: "This part of the model overlaps itself near the tool". Next action: "Run Make solid on it first, or move the tool".
- **Extra tests:**
  - `self_crossing_far_from_tool_doesnt_block_the_boolean` (a scan with a crossing on the far side, and a tool on the near side, gives a valid result);
  - `self_crossing_inside_the_window_is_refused_with_make_solid_next_action`.

### Task 8a: Make solid (voxel remesh), the Meshmixer route for messy scans

Meshmixer users run Make Solid before booleans on messy scans. This task gives MeshFixer the same explicit, user-chosen tool, and it is the next action in the `operand_overlaps_itself` refusal. It is never applied silently.

**Files:**
- Create: `crates/mf-operations/src/make_solid.rs`, `specs/operations/mesh.make_solid.yaml`
- Test: `crates/mf-operations/tests/make_solid.rs`

**Behaviour:**
- `mesh.make_solid` v1 works on a whole object or on a face-set region. For a region, the remeshed patch is stitched back along the region boundary. If that can't be done cleanly, it is refused by name.
- The algorithm:
  1. Compute a signed distance field, or a generalised winding number for open or self-intersecting input, on a sparse grid.
  2. Extract the surface with dual contouring or marching cubes at that resolution.
  3. Optionally smooth sharp features.
  4. The output is closed, manifold and has no self-intersections, and it is validated by D4.
- Params:
  - `cell_size: float mm` (default: 1/256 of the bbox diagonal);
  - `offset: float mm` (0);
  - `preserve_sharp: bool` (true).
- Deterministic across thread counts, so replay is Exact on one OS.
- Cancellable, with stages "measuring", "building the field", "extracting the surface" and "checking the result".
- Memory is bounded by a sparse grid. Refused by name if the cell count would exceed the budget, with the next action "use a larger cell size". It does not silently coarsen.
- The UI states the trade-off in plain language: "Rebuilds the surface as one closed solid; fine detail smaller than the cell size is lost."

**Tests:**
- `make_solid_turns_self_intersecting_scan_into_valid_solid` (winged victory region, and a corpus model);
- `make_solid_output_is_within_cell_size_of_input` (Hausdorff ≤ 1 cell on a sphere);
- `make_solid_is_deterministic_at_1_and_n_threads`;
- `make_solid_refuses_over_budget_grid_with_next_action`;
- `make_solid_cancel_leaves_document_unchanged`;
- `boolean_after_make_solid_succeeds_where_it_was_refused` (the end-to-end route).

**Commit:** `feat(ops): mesh.make_solid voxel remesh [MF-MODEL-001]`

### Task 9: Intersection analysis (`mesh.intersections`)

**Files:**
- Create: `crates/mf-geometry/src/intersect.rs`, `crates/mf-operations/src/intersections.rs`, `specs/operations/mesh.intersections.yaml`
- Test: `crates/mf-geometry/tests/intersect.rs`

**Interfaces:**
- **Consumes:** `Bvh` (D17), `robust::orient3d`, `operand_in_frame`.
- **Produces:**
  - `tri_tri_intersect(a: [[f64; 3]; 3], b: [[f64; 3]; 3]) -> TriTri`, with `enum TriTri { Disjoint, Touching, Crossing }`. Exact (Devillers–Guigue with orient3d; the coplanar case uses orient2d). Touching, sharing only a vertex or edge, is reported separately and isn't counted as an intersection.
  - `intersections_between(a: &Mesh, a_bvh: &Bvh, b: &Mesh, b_bvh: &Bvh, b_to_a: &DMat4, ctx) -> Result<IntersectionReport, Cancelled>`.
  - `self_intersections(a: &Mesh, bvh: &Bvh, ctx)`. It skips pairs that share a vertex unless they cross. This is shared with MF-ISSUE-049 (M5): the M5 operation calls this function.
  - `struct IntersectionReport { regions: Vec<IntersectionRegion>, pair_count: u64 }` and `struct IntersectionRegion { faces_a: Vec<FaceId>, faces_b: Vec<FaceId>, extent: f64, centroid: [f64; 3], segments: Vec<[[f64; 3]; 2]> }`.
    - A region is a connected group of intersecting faces.
    - Segments are f64 constructions for display only, never used for decisions.
  - `mesh.intersections` v1: read-only, with optional operand `other`. Without it, the result is self-intersections.
- [ ] **Step 1:** Tests:
  - `tri_tri_exact_cases` (crossing, touching at a vertex or edge, coplanar overlap, 1e-300-scale near-misses);
  - `intersections_found_regardless_of_thread_count`;
  - `detection_agrees_with_boolean_no_overlap` (whenever detection reports none, `mesh.boolean` reports `no_overlap`);
  - `self_intersections_found_on_messy_self_intersecting_export`.
  - Timing (heavy):
    - `intersections_30m_vs_50k_under_1s`;
    - `self_intersections_5m_cancellable`.
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(geometry): exact intersection analysis [MF-MODEL-002]`.

### Task 10: Intersections and booleans in the desktop

**Files:** `crates/mf-desktop/src/{app.rs, panels.rs, defects.rs, worker.rs, theme.rs}`, `docs/design/visual-language.md`. Test: `crates/mf-desktop/tests/booleans.rs`

**Behaviour:**
- **Finding intersections.** While two or more objects' world bounds overlap, the analysis worker runs `intersections_between` on the existing picking BVHs. It reruns after each transform commit, never during a drag.
- **The tree.** An **Intersections (n)** category, in `INTERSECTION`, appears under each object involved. Rows read "Intersection 1 · with plate · 340 faces · 12.3 mm". They behave like every other row: click, double-click to frame, hover.
- **The overlay.** Intersection segments draw as 3 pt lines, and the faces involved get a light `INTERSECTION` tint (a new state bit in `FaceStates`).
- **Properties for an intersection, or for two objects selected** (Shift-click):
  - "Scan and plate overlap", followed by **Union**, **Subtract plate from scan** (with a ⇄ swap), **Keep the overlap** (intersect) and **Move apart** (opens Transform on the second object).
  - A "Keep plate" checkbox (`keep_tool`).
- **Refusals** show their copy and their next-action button. "Fill all holes, then combine" runs both commands.
- **Status line.** "Combined scan and plate: 2,314 new faces; 5,112,004 unchanged".

- [ ] **Step 1:** GUI tests:
  - `overlapping_objects_list_an_intersection_with_union_subtract_move_apart`;
  - `union_from_properties_removes_the_tool_and_undo_restores_it`;
  - `open_target_shows_fill_then_combine`;
  - `intersection_overlay_survives_transform_tool`.
  - Timing (heavy): `ui_frames_under_50ms_during_30m_union`.
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(desktop): intersections and booleans from the target [MF-MODEL-001, MF-MODEL-002]`.

### Task 11: Checkpoint P1-B (capture and continue)

- [ ] Capture, as for Checkpoint P1-A:
  - two overlapping objects with the intersection overlay;
  - Union, Subtract and Intersect;
  - a refusal on an open scan, with the one-click fix;
  - undo;
  - a 30M scan union with progress and cancel.
- [ ] Put them in `artifacts/phase1/p1-m2/` and continue.

**Exit criteria (P1-M2):**
- **MF-MODEL-001:** every messy-corpus case is valid or a named refusal; 1M × 1M union ≤ 10 s; 30M × 50k ≤ 3 s; cancel ≤ 250 ms.
- **MF-MODEL-002:** overlay and list, with next actions.
- Provenance is total over operands.
- Replay is Exact, and Equivalent in the cross-OS job.
- The checkpoint captures are in `artifacts/phase1/` (the owner reviews them later, without blocking).

---

# P1-M3: Split with alignment pegs and joints (MF-MODEL-003)

**Meshmixer benchmark:** Edit ▸ Plane Cut (keep one side, or slice into both) and Edit ▸ Separate Shells. Meshmixer has no alignment features: people add cylinders and subtract them by hand. PrusaSlicer's cut tool has connectors (plugs and dowels, with tolerance).

**Exceed:**
- every part is guaranteed closed and manifold, or the cut is refused;
- pegs, keyed pegs and dovetails come with a printing clearance that is **measured on the meshes**;
- automatic placement says when a peg won't fit and what size would;
- parts are separate 3MF objects;
- it works on 30M scans;
- it replays Exact.

### Task 12: `mesh.split_plane` (exact plane cut with caps)

**Files:**
- Create: `crates/mf-boolean/src/plane_cut.rs`, `crates/mf-operations/src/split.rs`, `specs/operations/mesh.split_plane.yaml`
- Test: `crates/mf-operations/tests/split.rs`, `tests/geometry/split.yaml`

**Interfaces:**
- **Consumes:** `triangulate_section` (Task 8), `validate_solid`, `ObjectChange::Create`.
- **Produces:**
  - `plane_cut(mesh: &Mesh, plane: Plane, ctx) -> Result<CutParts, CutError>`, with:
    - `Plane { point: [f64; 3], normal: [f64; 3] }` in object space; vertex sides are decided with `orient3d` on three exact points built from the plane;
    - `CutParts { above: (Mesh, Provenance), below: (Mesh, Provenance), section_regions: Vec<SectionRegion> }`;
    - `SectionRegion { outer: Vec<[f64; 2]>, holes: Vec<Vec<[f64; 2]>>, frame: PlaneFrame }`, which P1-M3's peg placement uses.
  - `mesh.split_plane` v1:
    - params: `plane_point: vec3 mm` and `plane_normal: vec3` (world space, converted to object space at execute time with the recorded transform), `keep: choice [both, above, below]`, `cut_gap: float mm` (0; D23: removes a slab of this thickness centred on the plane, so the parts don't touch), plus the peg params of Task 13 (default `none`).
    - The above part keeps the target's id and name ("scan (top)"). The below part is `Create`d as "scan (bottom)".
    - With `keep: above` or `below`, the other part is discarded and its faces are listed as removed.
    - The report lists each part's cap faces (`cap_faces`), so a later `mesh.delete_faces` can target them (UC2).
  - **Refusals:**
    - `operand_not_closed`: "Close holes first";
    - `plane_misses_object`;
    - `section_self_intersects`: "The cut grazes the surface here; move the plane slightly".
- [ ] **Step 1:** Tests:
  - `cut_cube_gives_two_closed_parts_with_exact_volumes`;
  - `vertex_exactly_on_plane_is_shared_not_duplicated`;
  - `nested_sections_cap_with_holes` (a hollow sphere);
  - `horizontal_vertical_and_bias_cuts_all_give_closed_parts` (normals +Z, +X, and (1, 0, 1)/√2 rotated 20° about Z);
  - `cut_gap_leaves_exactly_that_gap_between_parts` (the minimum distance between the parts is `cut_gap` to 1e-9 relative);
  - `faces_away_from_the_plane_are_kept_bitwise`;
  - `split_replays_exact`;
  - `cancel_at_every_stage_leaves_document_unchanged`.
  - Timing (heavy): `split_5m_under_2s`, `split_30m_under_10s`.
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(operations): exact plane split with capped parts [MF-MODEL-003]`.

### Task 12a: Print fit profile (D20)

**Files:**
- Create: `crates/mf-parts/src/{fit.rs, dirs.rs}`, `crates/mf-desktop/src/settings.rs`
- Modify: `specs/operation.schema.json` (`default_from`), `crates/mf-spec/src/model.rs`, `crates/mf-cli/src/main.rs` (`--fit-profile`), `crates/mf-desktop/src/panels.rs` (Settings menu)
- Test: `crates/mf-parts/tests/fit.rs`, `crates/mf-cli/tests/cli.rs`

**Interfaces:**
- **Produces:**
  - `struct FitProfile { name: String, peg_clearance: f64, length_allowance: f64, hinge_gap: f64, clasp_interference: f64 }`, in mm. `FitProfile::builtin()` is "Default FDM, 0.4 mm nozzle": 0.2 / 0.4 / 0.4 / 0.3.
  - `struct FitProfiles { active: String, profiles: Vec<FitProfile> }`, with `FitProfiles::load(dir) -> Result<FitProfiles, FitError>` and `save(&self, dir)`. A malformed file is a named error shown in Settings; the built-in is never edited.
  - `fn config_dir() -> Result<PathBuf, DirsError>`, in the marked adapter module `dirs.rs`.
  - `ParamSpec.default_from: Option<String>` (`"fit.peg_clearance"`, …), with `OperationSpec::fill_defaults(&self, raw: &mut BTreeMap<String, String>, fit: &FitProfile)`. It fills only parameters the caller didn't set, before validation, so the recorded params hold the values used.
  - **Settings ▸ Print fit:** a profile picker, Duplicate, Rename, Delete, and the four fields with their units and one-line explanations, for example "Dowel is this much thinner than its hole".
- [ ] **Step 1:** Tests:
  - `builtin_profile_has_the_documented_defaults`;
  - `explicit_param_beats_profile_default`;
  - `recorded_params_hold_profile_values`;
  - `replay_ignores_the_active_profile` (record with a 0.3 mm profile, replay with the built-in: Exact);
  - `malformed_profile_file_is_a_named_error`;
  - CLI `fit_profile_flag_sets_defaults`.
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(parts): print fit profiles as operation defaults [D20]`.

### Task 13: Alignment holes and dowels (round, square, keyed) with fit clearance

**Files:** `crates/mf-boolean/src/primitives.rs` (new), `crates/mf-operations/src/split/pegs.rs` (new), `crates/mf-operations/src/layout.rs` (new), `specs/operations/{mesh.split_plane.yaml, object.arrange_on_plate.yaml}`. Test: `crates/mf-operations/tests/pegs.rs`

**Interfaces:**
- **Consumes:** `CutParts.section_regions`, `mf_boolean::boolean` (Task 8), `FitProfile` defaults (Task 12a).
- **Produces:**
  - `primitives::{cylinder, box_prism, keyed_prism, dovetail, chamfered_dowel}(…, chord_tol) -> Mesh`, closed and outward.
  - `pole_of_inaccessibility(region: &SectionRegion, precision: f64) -> ([f64; 2], f64)`.
  - `place_holes(regions, shape, diameter, count: u32 /* 0 = automatic */, min_wall) -> Result<Vec<HolePlacement>, PegError>`, with `PegError::DoesNotFit { region, widest_fit_mm }`.
  - `dowel_size(hole_diameter, depth_above, depth_below, peg_clearance, length_allowance) -> (f64 /* diameter */, f64 /* length */)`, which is exactly D12's formulas. A length ≤ 0 is refused as `dowel_too_short`.
  - `layout_on_plate(occupied: &[Footprint], new: &[Footprint], plate: &Plate, spacing: f64) -> LayoutResult { transforms: Vec<[f64; 12]>, off_plate: Vec<usize> }` (D21). `Footprint` is the world XY bounding rectangle.
  - Split params:
    - `pegs: choice [none, dowels, attached, dovetail]` (none);
    - `peg_shape: choice [round, square, keyed]` (round);
    - `hole_diameter: float mm` (5);
    - `hole_depth_above: float mm` (6), `hole_depth_below: float mm` (6);
    - `peg_count: integer` (0 = automatic);
    - `peg_clearance: float mm`, `default_from: fit.peg_clearance`;
    - `length_allowance: float mm`, `default_from: fit.length_allowance`;
    - `min_wall: float mm` (1.5).
  - **`pegs: dowels`:**
    - holes are subtracted from both parts at the same placements;
    - each dowel is `Create`d as "scan dowel 1"…, standing upright, placed by `layout_on_plate` and dropped to the plate;
    - `off_plate` entries are reported in words ("2 dowels didn't fit on the plate; they're placed beside it").
  - **`pegs: attached`:** the peg (hole diameter − clearance, length `hole_depth_below − length_allowance` above the face) is unioned onto the above part, and the hole is subtracted from the below part.
  - **`object.arrange_on_plate` v1:** targets the whole document, or the given objects as operands, and lays them out with D21.
- [ ] **Step 1:** Tests:
  - `dowel_size_matches_owner_example`: holes 5 mm and 8 mm deep, allowance 0.4 mm, give a 12.6 mm dowel; a 5 mm hole with 0.2 mm clearance gives a 4.8 mm dowel;
  - `dowels_measured_on_meshes_fit_their_holes`: in each part, the hole's measured diameter minus the dowel's is `peg_clearance` ± the chord tolerance, and the hole depths are measured as requested;
  - `dowels_are_separate_objects_on_the_plate_not_overlapping_anything` (world min z = 0, and no footprint overlaps);
  - `every_part_is_closed_and_manifold_after_holes`;
  - `auto_places_two_holes_in_a_wide_section_and_one_keyed_in_a_narrow_one`;
  - `too_thin_section_refuses_with_widest_fit`;
  - `default_clearance_is_0_2_mm_in_a_metre_document` (Review Focus 4);
  - `full_plate_reports_dowels_off_the_plate`;
  - `pegs_replay_exact`.
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(operations): alignment holes and dowels from the fit profile [MF-MODEL-003]`.

### Task 14: Dovetail joint, and split by selected region

**Files:** `crates/mf-operations/src/split/{dovetail.rs, region.rs}`, `specs/operations/mesh.split_region.yaml`. Test: `crates/mf-operations/tests/split_region.rs`

**Interfaces:**
- **Produces:**
  - `pegs: dovetail`: a trapezoid prism along each region's principal axis (angle 15°, fixed in v1 and recorded), with `peg_clearance` applied to its flanks.
  - `mesh.delete_faces` v1 (Meshmixer's "Discard"): target a face set; it removes those faces and any vertices left unused. The desktop runs it with the Delete key on a face selection. UC2 uses it to delete the neck's cut cap.
  - `mesh.split_region` v1:
    - target: a face set (selection);
    - params: `cap_style: choice [flat, smooth]` (smooth).
    - The selected faces become a new object, and both boundaries are closed with `fill_holes`, reusing the existing patch code and its 1000-edge algorithm limit (MF-ISSUE-043 is cited in that refusal).
- [ ] **Step 1:** Tests:
  - `dovetail_parts_slide_together_with_clearance`;
  - `split_region_gives_two_closed_parts_and_keeps_faces`;
  - `region_boundary_over_1000_edges_refuses_citing_issue_043`;
  - `delete_faces_leaves_an_open_boundary_listed_as_a_hole`.
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(operations): dovetail joints and split by region [MF-MODEL-003]`.

### Task 15: The Split tool in the desktop

**Files:** `crates/mf-desktop/src/{split_tool.rs (new), app.rs, panels.rs}`, `crates/mf-render/src/overlay.rs` (cut plane). Test: `crates/mf-desktop/tests/split.rs`

**Behaviour:**
- **Opening it.** Properties ▸ **Split** on an object shows a cut plane through the bounding-box centre, normal to Z. The plane is moved and tilted with the Task 1 gizmo, which is reused for the plane.
- **The preview.** A section outline is drawn on the plane, computed on the worker from a slab query of the BVH and refreshed on release. Peg positions are previewed as discs.
- **Placing the plane.**
  - The presets **Horizontal**, **Vertical (front)** and **Vertical (side)** set the plane at the bounding-box centre.
  - **Typed entry** gives the point in document units, plus Tilt (from horizontal) and Direction (around Z), in degrees. A bias cut is any tilt.
  - The gizmo and the fields stay in sync. Only Apply records `plane_point` and `plane_normal`.
- **Controls.**
  - Alignment: None, Dowels, Attached pegs or Dovetail.
  - Shape, hole diameter, both hole depths, count ("Automatic").
  - Read-only lines from the fit profile: "Dowel 4.8 × 11.6 mm · clearance 0.2 mm · allowance 0.4 mm (from Print fit: Default)", with an **Edit fit…** link.
  - Gap between parts, then Apply.
- **After the split.** The parts appear in the tree as "scan (top)" and "scan (bottom)". An **Explode** view toggle moves them apart along the normal in the view only, with no command.
- **A face-set selection** offers **Split selection off**.

- [ ] **Step 1:** GUI tests:
  - `split_tool_previews_section_and_pegs_then_creates_two_parts`;
  - `horizontal_vertical_presets_and_typed_bias_set_the_plane`;
  - `fit_profile_values_show_and_are_recorded`;
  - `peg_does_not_fit_shows_widest_fit_in_plain_language`;
  - `explode_is_view_only`.
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(desktop): split tool with peg preview [MF-MODEL-003]`.

### Task 16: Checkpoint P1-C and a print check (capture and continue)

- [ ] Capture, as before:
  - a plane cut on the real scan;
  - automatic pegs;
  - a refusal for a thin section;
  - explode;
  - 3MF export.
- [ ] Stage the print check for the owner (non-blocking): the UC1 3MF in `artifacts/phase1/p1-m3/print-check/`, a 60 mm sphere split in half with two 5 mm dowels from the default fit profile, plus instructions. The owner records photos in `docs/milestones/p1-m3.md` when reviewing.
- [ ] Put the captures in `artifacts/phase1/p1-m3/` and continue.

### Task 16a: UC1 acceptance scenario: split and pegs

**Files:** `crates/mf-cli/tests/uc1_split_pegs.rs`, `crates/mf-desktop/tests/uc1_capture.rs`, `scripts/capture-uc1.sh`

- [ ] **Step 1: Scenario test (CLI, end to end)** `uc1_split_horizontal_vertical_bias_with_dowels`. On the procedural 60 mm sphere and on the real scan (closed with `fill_holes` first):
  - `meshfixer run … --op mesh.split_plane` three times: horizontal, vertical and a 30° bias. Each run uses `--param pegs=dowels hole_depth_above=5 hole_depth_below=8` and `--fit-profile Default`, then exports 3MF.
  - **Assert:**
    - every part and dowel is closed and manifold;
    - dowel length = 5 + 8 − 0.4 mm and diameter = hole − 0.2 mm, measured on the meshes;
    - the dowels are separate 3MF objects with world min z = 0, not overlapping any object's footprint;
    - the recorded params hold the profile's values;
    - `meshfixer replay` gives all-Exact with a different active profile.
- [ ] **Step 2: GUI capture** `uc1_capture`, through kittest at 1440×900: Split tool, the presets, a gizmo-tilted bias, typed entry, the dowel preview, Apply, and the dowels on the plate. Saved to `artifacts/phase1/p1-m3/uc1/` (PNGs plus MP4).
- [ ] **Step 3:** FAIL → fix whatever the scenario exposes → PASS.
- [ ] **Step 4:** Commit `test: UC1 split-and-pegs acceptance scenario`.

**Exit criteria (P1-M3):**
- UC1 passes (Task 16a).
- MF-MODEL-003's acceptance: clearance is measured on the meshes, and every part is closed and manifold.
- Pegs, keyed pegs and dovetails work.
- The region split works.
- A 30M split takes ≤ 10 s.
- The print check is queued for the owner's later review (it doesn't block).
- The checkpoint captures are in `artifacts/phase1/` (the owner reviews them later, without blocking).

---

# P1-M4: Sculpting and carving (MF-SCULPT-001…004)

**Meshmixer benchmark:** Sculpt ▸ Volume brushes (Draw, Inflate, Flatten, Drag, Pinch, Move, smoothing) with adaptive refinement, symmetry and freeze. Meshmixer slows down noticeably on multi-million-triangle scans.

**Exceed:**
- a dab reaches the screen within one frame at ≥ 60 fps on 5M, and ≥ 30 fps on 30M;
- each stroke is one command that replays Exact;
- nothing outside the brush footprint changes (MF-OP-003 per stroke), and a test proves it;
- the history of 1,000 strokes stays within 1.5× of one revision plus the footprints.

### Task 16b: Sculpting enablers from the M4 final review (before Task 17)

The M4 technical review found two problems, I1 and I2 in `.superpowers/sdd/2026-11-08-m4-scale-reliability-ux/final-review-technical.md`. Both block per-stroke editing.
- **Fix 1: re-upload only touched chunks.** Today, moving vertices re-uploads every GPU chunk that shares a vertex chunk. It must re-upload only the face chunks that contain a face touching a moved vertex.
  - Test `one_percent_dab_reuploads_only_touched_face_chunks`, on the real scan.
- **Fix 2: decouple storage and GPU chunk sizes.** The copy-on-write storage chunk becomes small (≤ 4,096 elements), and the GPU face chunk stays large. That way a 1,000-stroke history fits the memory budget, and buffers and draw calls do not multiply.
  - Test `thousand_small_strokes_history_within_budget`.

### Task 17: The stroke engine: brushes, falloff, symmetry

**Files:**
- Create: `crates/mf-operations/src/sculpt/{mod.rs, brush.rs, stroke.rs, falloff.rs}`, `specs/operations/mesh.sculpt_stroke.yaml`
- Test: `crates/mf-operations/tests/sculpt.rs`

**Interfaces:**
- **Consumes:** `Bvh` (D17), `MeshEdit`.
- **Produces:**
  - `enum BrushKind { Smooth, Inflate, Deflate, Flatten, Drag, Pinch, Crease, Carve }`.
  - `struct BrushParams { kind, radius: f64 /* world, document units */, strength: f64, falloff: Falloff, symmetry: [bool; 3], pivot: [f64; 3], limit_to: Option<Vec<FaceId>>, frozen: Vec<FaceId>, view_dir: [f64; 3] }`.
  - `struct DabSample { face: FaceId /* input revision */, bary: [f64; 3], pressure: f64 }`.
    - Each dab's centre is that point on the input mesh, projected along `view_dir` onto the current mid-stroke surface.
    - That makes the path remappable by provenance (MF-HIST-002) and never a raw position.
  - `struct StrokeState`, created by `StrokeState::new(base: &Mesh, bvh: &Bvh, params: BrushParams)`, with:
    - `apply(&mut self, dab: &DabSample, ctx) -> Result<DabDelta, Cancelled>`, where `DabDelta { face_chunks: Vec<usize>, vertex_chunks: Vec<usize> }`;
    - `preview_mesh(&self) -> Mesh`, cheap because it shares chunks;
    - `finish(self) -> (Mesh, Provenance)`.
  - `mesh.sculpt_stroke` v1:
    - target: whole object or a face set (`limit_to`);
    - params: `brush: choice`, `radius: float mm`, `strength: float`, `falloff: choice [smooth, sphere, sharp, flat]`, `symmetry_x`, `symmetry_y`, `symmetry_z: bool`, `pivot: vec3`, `view_dir: vec3`, `samples` (list of `DabSample`, carried as a new `ParamKind::Samples` serialized as an array; added to the schema in this task), and `detail` (Task 18).
    - Executing the operation replays the samples in order through `StrokeState`. The live preview uses the same code, so the commit and the replay are identical.
- [ ] **Step 1:** Tests:
  - `each_brush_moves_only_vertices_within_radius` (untouched outside the footprint, bitwise);
  - `frozen_faces_never_move`;
  - `symmetry_x_mirrors_displacement_exactly`;
  - `live_stroke_equals_replayed_stroke_hash`;
  - `stroke_is_strict_at_1_and_n_threads`;
  - `samples_remap_across_an_upstream_fill` (HIST-002).
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(operations): sculpt stroke engine and brushes [MF-SCULPT-001, MF-SCULPT-003]`.

### Task 18: Local dynamic topology (detail where it's needed)

**Files:** `crates/mf-operations/src/sculpt/remesh.rs` (new). Test: `crates/mf-operations/tests/sculpt_remesh.rs`

**Interfaces:**
- **Produces:** `remesh_region(edit: &mut MeshEdit, region: &RegionAdjacency, target_edge: f64, ctx) -> Result<RemeshStats, Cancelled>`.
  - It splits edges longer than 4/3 of the target, collapses edges shorter than 4/5 of it, and flips to improve valence.
  - It works only on faces wholly inside the footprint.
  - `RegionAdjacency` is built on demand from the base topology plus the edit, so no global topology is rebuilt per dab.
  - Stroke param `detail: float` (relative target edge, 0 = off) and `detail_mode: choice [off, adaptive]`.
  - P1-M5's blend reuses this (D13), so it's a public function: `pub fn remesh_band(mesh: &Mesh, faces: &[FaceId], target_edge: f64, ctx) -> Result<(Mesh, Provenance), OpError>`.
- [ ] **Step 1:** Tests:
  - `remesh_keeps_edges_within_target_band`;
  - `remesh_touches_nothing_outside_the_footprint`;
  - `remesh_keeps_the_mesh_manifold_and_closed`;
  - `remesh_provenance_is_total`.
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(operations): local dynamic remeshing [MF-SCULPT-002]`.

### Task 19: Live strokes in the viewport

**Files:** `crates/mf-desktop/src/sculpt_tool.rs` (new), `crates/mf-desktop/src/{worker.rs, app.rs, panels.rs}`, `crates/mf-render/src/{gpu_mesh.rs, bvh refit in mf-geometry/src/bvh.rs}`. Test: `crates/mf-desktop/tests/sculpt.rs`

**Interfaces:**
- **Consumes:** `StrokeState`, `ChunkVertices::build`, `GpuMesh::sync`.
- **Produces:**
  - A stroke worker owns its `StrokeState` and receives `DabSample`s over a channel. Dab spacing is 25% of the radius, sampled from the pointer at the frame rate.
  - For each dab it rebuilds only `DabDelta`'s chunks, uploads them, and refits the BVH leaves under the footprint (`Bvh::refit(&mut self, mesh, faces)`), so the next dab picks the moved surface.
  - Releasing the pointer commits `mesh.sculpt_stroke` with the recorded samples. The stroke worker's mesh is handed over as the result, with no recomputation; a debug assertion re-runs it in tests.
  - Esc mid-stroke drops the stroke.
  - **Brush panel** in Properties: brush buttons, radius (`[` and `]`), strength, falloff, symmetry (drawn as a `CUT_PLANE`-style plane through the pivot), Detail, and "Limit to selection".
  - **Freeze.** **Freeze** paints a frozen face set: `object.set_frozen` is a document command like `set_openings`, and frozen faces draw with the hatch (D9). Holding Shift unfreezes.
  - **Pen pressure** follows D19.
- [ ] **Step 1:** GUI tests:
  - `one_stroke_is_one_undo`;
  - `escape_drops_the_stroke` (also `cancel_at_every_stage_leaves_document_unchanged` for a stroke, Review Focus 3);
  - `freeze_hatch_and_frozen_faces_do_not_move`;
  - `brush_cursor_is_a_hover_outline`.
  - Timing (heavy):
    - `dab_reaches_screen_within_one_frame_at_5m_60fps`;
    - `stroke_at_30m_keeps_30fps`;
    - `stroke_uploads_only_touched_chunks` (upload byte counters, MF-RENDER-003).
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(desktop): live sculpting with per-dab chunk uploads [MF-SCULPT-001, MF-SCULPT-004]`.

### Task 20: Stroke history memory, and Checkpoint P1-D (capture and continue)

- [ ] Test (heavy): `thousand_strokes_stay_within_1_5x_plus_footprints` on 5M (MF-SCULPT-004, MF-DOC-002).
- [ ] Capture:
  - each brush on the real scan;
  - symmetry;
  - freeze;
  - detail on and off;
  - undo;
  - a 30M stroke with the frame-time overlay.
- [ ] Put them in `artifacts/phase1/p1-m4/` and continue.

**Exit criteria (P1-M4):**
- MF-SCULPT-001…004 are met (carve as a brush; boolean carving is P1-M2's subtract).
- 5M ≥ 60 fps per dab, and 30M ≥ 30 fps.
- 1,000-stroke memory is within budget.
- Replay is Exact.
- The checkpoint captures are in `artifacts/phase1/` (the owner reviews them later, without blocking).

---

# P1-M5: Mixing and kitbashing (MF-MODEL-004)

**Meshmixer benchmark:** Meshmix, Meshmixer's signature tool. You drag a part from the library onto a surface, where it orients to the normal, then transform it and Accept. Solid parts are joined; open "stamp" parts are zippered and blended in.

**Exceed:**
- the join is guaranteed valid, or refused with the reason (D4, D14);
- blends are faired (C1) and their width is recorded;
- parts carry snap points and licences;
- user and plugin parts share one manifest;
- it works on 30M scans with local cost (D5);
- every placement and join replays Exact.

### Task 21: The parts library (`mf-parts`) and `object.add_part`

**Files:**
- Create: `crates/mf-parts/{Cargo.toml, src/lib.rs, src/manifest.rs, src/builtin.rs, src/user.rs}`, `crates/mf-operations/src/add_part.rs`, `specs/operations/object.add_part.yaml`
- Test: `crates/mf-parts/tests/library.rs`

**Interfaces:**
- **Produces:**
  - `struct PartManifest { id: String, name: String, version: u32, licence: String, source: String, kind: PartKind, attach: Attach, default_scale: f64 }`.
  - `enum PartKind { Solid, Open }`.
  - `struct Attach { point: [f64; 3], normal: [f64; 3], up: [f64; 3] }`, in part space. The normal points out of the target surface.
  - `struct Library { parts: Vec<LibraryPart> }`, built by `Library::load(user_dir: Option<&Path>, ctx) -> (Library, Vec<LibraryWarning>)`. A malformed user part is listed as a warning by name, never skipped silently.
  - `LibraryPart::mesh(&self, ctx) -> Result<Mesh, PartError>`, with `LibraryPart::content_hash()`. A built-in's hash covers its generator id, version and parameters.
  - `save_object_as_part(mesh, manifest, dir) -> Result<PathBuf, PartError>`.
  - `struct SurfaceFrame { point: [f64; 3], normal: [f64; 3], tangent: [f64; 3] }`, from `SurfaceFrame::at(mesh: &Mesh, transform: &[f64; 12], face: FaceId, bary: [f64; 3], radius: f64) -> SurfaceFrame`.
    - The normal is area-weighted over faces within `radius`, so it stays stable on noisy scans.
    - The tangent comes from the camera's up vector, projected.
  - `place(attach: &Attach, frame: &SurfaceFrame, scale: f64, spin_deg: f64) -> [f64; 12]`. Connectors (MF-MODEL-005) reuse `SurfaceFrame` and `place`.
  - `object.add_part` v1:
    - params: `part_id`, `part_hash`, `transform: affine`;
    - creates an object named after the part.
    - The part's source is recorded like an import, so replay verifies its hash.
- [ ] **Step 1:** Tests:
  - `builtins_are_closed_and_deterministic`;
  - `malformed_user_part_is_a_named_warning`;
  - `place_aligns_attach_normal_with_surface_normal`;
  - `surface_frame_is_stable_on_a_noisy_scan` (normal within 5° under 1% noise);
  - `add_part_replays_exact_and_verifies_part_hash`.
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(parts): part library, surface frames and add_part [MF-MODEL-004]`.

### Task 22: Join and blend (`mesh.boolean` with `blend_width`; open parts)

**Files:** `crates/mf-operations/src/{boolean.rs, blend.rs (new)}`, `crates/mf-operations/src/fill_holes/fairing.rs` (expose `fair_region`). Test: `crates/mf-operations/tests/blend.rs`

**Interfaces:**
- **Consumes:** `remesh_band` (Task 18), `mf_boolean::boolean` (Task 8), `fill_holes` (flat, for capping open parts).
- **Produces:**
  - `pub fn fair_region(mesh: &Mesh, free: &[VertexId], style: Style, ctx) -> Result<Result<(Mesh, Provenance), FairFailed>, Cancelled>`, which generalizes `fair` from patch interiors to any vertex set with two fixed rings.
  - When `blend_width > 0`, `mesh.boolean` v1 continues after the union:
    - the band is every vertex within `blend_width` (edge-path distance) of the seam;
    - the band is remeshed to the seam's mean edge length;
    - the result is faired (smooth), then validated.
    - Union only: other operations with `blend_width > 0` are refused by name.
  - **Open parts** (`PartKind::Open`) are capped (flat) and sunk by `embed_depth`, a float in mm whose default is `0.5 × blend_width`, along −normal before the union (D14). The cap's faces never survive the union, and a test checks that.
  - **Refusals:**
    - `blend_self_intersects`: "The blend folds over itself; try a narrower blend";
    - `blend_did_not_converge`, named, with no fallback to a sharp seam.
- [ ] **Step 1:** Tests:
  - `blended_union_is_closed_manifold_and_c1_across_the_band` (dihedral angles across the band's edge ≤ the sharp seam's);
  - `open_stamp_part_blends_without_leaving_its_cap`;
  - `blend_on_30m_scan_touches_only_the_window` (heavy; `untouched_outside_target`);
  - `blend_replays_exact`;
  - `too_wide_blend_refuses_by_name`.
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(operations): seam blend for mixing [MF-MODEL-004]`.

### Task 23: The Parts tool in the desktop: drag, drop, place, join

**Files:** `crates/mf-desktop/src/parts_tool.rs` (new), `crates/mf-desktop/src/{app.rs, panels.rs, worker.rs}`. Test: `crates/mf-desktop/tests/mixing.rs`

**Behaviour:**
- **The library.** The left panel gains a **Parts** section below the objects, with thumbnails rendered offscreen on a worker at load and cached. It has search and a "Your parts" group.
- **Dragging a part into the view.** The part's render mesh is uploaded once, when the drag starts. It follows the cursor at the `SurfaceFrame` under it, ray-picked through the existing BVH, with a `HOVER` glow and no geometry work. Over empty space it hides and the status reads "Drop onto a model".
- **Dropping it** commits `object.add_part`, and the Transform tool opens on the new part, with spin about the attach normal as the default rotate handle.
- **Properties for a placed part over a target:**
  - "Join to scan": **Keep separate**, **Merge** (union), **Blend** (with a width slider in mm and "Blend width 2.0 mm").
  - An open stamp part shows "Open part: it will be closed and sunk 1.0 mm into the surface".
  - Refusals carry their next actions, for example "Fill all holes, then join" (D15).
- **Saving parts.** **Save as part…** on any object writes it to the user library, with the attach point taken from where you click on it.

- [ ] **Step 1:** GUI tests:
  - `drag_part_previews_on_surface_aligned_to_normal`;
  - `drop_then_merge_is_two_undoable_commands`;
  - `blend_from_properties_shows_progress_and_result_summary`;
  - `drop_over_empty_space_adds_nothing`;
  - `save_as_part_appears_in_your_parts`.
  - Timing (heavy):
    - `part_drag_preview_keeps_60fps_on_30m`;
    - `merge_30m_scan_with_50k_part_under_3s`;
    - `blend_under_2s`.
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(desktop): parts library, drag-to-surface placement and join [MF-MODEL-004]`.

### Task 24: Checkpoint P1-E (capture and continue)

- [ ] Capture:
  - drag a built-in part onto the real scan;
  - transform it;
  - Merge;
  - Blend;
  - an open stamp part;
  - undo;
  - a 30M scan with progress.
- [ ] Put them in `artifacts/phase1/p1-m5/` and continue.

### Task 24a: UC2 acceptance scenario: "Rocktopus"

**Files:** `crates/mf-fixtures/src/scenario.rs` (new: `octopus()`, `head()`, `hollow_box(wall)`, D24), `crates/mf-command/tests/uc2_rocktopus.rs`, `crates/mf-desktop/tests/uc2_capture.rs`, `scripts/capture-uc2.sh`

**Interfaces:**
- **Consumes:**
  - `mesh.split_plane` (Task 12), `object.delete` (Task 3), `mesh.delete_faces` (Task 14);
  - `object.set_transform` with `gizmo::compose` (Tasks 1 and 3);
  - `mesh.boolean` with `blend_width` (Task 22), and D15's local closure (Task 8);
  - `mesh.sculpt_stroke` (Task 17).
- **Produces:** `mf_fixtures::scenario::{octopus, head, hollow_box}`. They're seeded and closed. The octopus has a body, a narrower neck and eight tentacles. The head is an ellipsoid with brow and jaw bumps.

- [ ] **Step 1: Scenario test (commands, end to end)** `uc2_rocktopus`, run twice, as `capped_neck` and `open_neck`:
  1. Import the octopus. `mesh.split_plane` horizontally through the neck, `keep: both`.
  2. `object.delete` the head piece.
  3. Open-neck variant only: run `mesh.delete_faces` on the body's `cap_faces` from the split report, leaving the neck open.
  4. Import the head. `object.set_transform` with `compose(scale 1.3, rotate (0, 0, 25°), move onto the neck)`, as the gizmo would produce, with the head's base overlapping the neck by 2 mm.
  5. `mesh.boolean` union with `blend_width = 3 mm`.
  6. Two `mesh.sculpt_stroke`s: smooth, then inflate, with samples generated along the seam inside the blend band and the radius inside the band.
  - **Assert:**
    - the document has exactly one object, which is closed and manifold, and passes `validate_solid` (no self-intersections);
    - the seam band is faired: band vertices satisfy the discrete Δ² equations to 1e-8 relative (before the strokes), and the largest dihedral angle across the band is below the sharp, unblended union's on the same inputs;
    - nothing outside the blend window changes: every octopus face outside the window is `Kept` and bitwise equal through the join and both strokes (`untouched_outside_target`);
    - the open-neck variant gives a closed result through D15's local closure, with no temporary face left;
    - a variant with the head moved 10 mm off the neck is refused as `opening_not_covered`, and the document is unchanged;
    - the saved log replays all-Exact.
- [ ] **Step 2: GUI capture** `uc2_capture`, through kittest: Split, select and Delete the head piece, Import, gizmo scale, rotate and move onto the neck, Blend from Properties, then smooth and inflate strokes. Saved to `artifacts/phase1/p1-m5/uc2/` (PNGs plus MP4). The optional CC0 models are used when they've been fetched (D24).
- [ ] **Step 3:** FAIL → fix → PASS.
- [ ] **Step 4:** Commit `test: UC2 Rocktopus acceptance scenario`.

**Exit criteria (P1-M5):**
- UC2 passes (Task 24a): capped and open neck.
- MF-MODEL-004 is met: library, snap points, attach, merge and blend.
- The scale targets above are met.
- Replay is Exact.
- The checkpoint captures are in `artifacts/phase1/` (the owner reviews them later, without blocking).

---

# P1-M6: Parametric connectors (MF-MODEL-005)

**Benchmark:** Meshmixer has no connectors; people download hinge models and union them by hand. The closest tools are parametric hinge generators (OpenSCAD and Thingiverse customizers) and PrusaSlicer's cut connectors.

**Exceed:**
- connectors are generated in the app, placed on the surface like parts, and resized without breaking their gaps (D22);
- gaps and interference come from the fit profile and are measured on the meshes;
- merged results are checked: moving parts stay separate;
- placements save to the library;
- the `Connector` trait is ready for plugins (MF-PLUG-001).

### Task 25: The `Connector` trait, connector objects and `object.add_connector`

**Files:**
- Create: `crates/mf-parts/src/connector.rs`, `crates/mf-operations/src/connector_ops.rs`, `specs/operations/{object.add_connector.yaml, object.edit_connector.yaml}`
- Modify: `crates/mf-document/src/lib.rs` (`Object.connector`)
- Test: `crates/mf-parts/tests/connector.rs`

**Interfaces:**
- **Consumes:** `SurfaceFrame`, `place` (Task 21); `primitives` (Task 13); `FitProfile` (Task 12a).
- **Produces:**
  - `trait Connector: Send + Sync { fn id(&self) -> &'static str; fn version(&self) -> u32; fn spec(&self) -> &'static OperationSpec /* params only */; fn generate(&self, params: &Params, ctx: &RunContext) -> Result<ConnectorParts, ConnectorError>; }`.
  - `struct ConnectorParts { parts: Vec<ConnectorPart> }` and `struct ConnectorPart { role: &'static str, mesh: Mesh, side: Side, join: JoinKind }`, with `enum Side { A, B, Neither }` and `enum JoinKind { Union, Subtract, Separate }`.
  - Connectors are registered in `mf_parts::connectors()`. The plugin host will add to the same registry.
  - `struct ConnectorInstance { connector: String, version: u32, params: Params, roles: Vec<(String, Side, JoinKind, Range<u32> /* face slots */)> }`, stored as `Object.connector: Option<ConnectorInstance>`.
  - `object.add_connector` v1: params `connector: choice` (from the registry), `connector_params` (the connector's own params, validated against its spec), and `transform: affine`. It creates one object with one component per part.
  - `object.edit_connector` v1 regenerates the mesh with new params and keeps the transform. The connector's frame is its hinge axis or clasp centre, so a resize stays in place.
- [ ] **Step 1:** Tests:
  - `connector_parts_are_closed_and_roles_cover_every_face`;
  - `edit_connector_keeps_transform_and_gaps`;
  - `connector_params_take_fit_profile_defaults_and_are_recorded`;
  - `add_connector_replays_exact`.
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(parts): connector trait and connector objects [MF-MODEL-005]`.

### Task 26: Print-in-place pin hinge

**Files:** `crates/mf-parts/src/connectors/hinge.rs`. Test: `crates/mf-parts/tests/hinge.rs`

**Interfaces:**
- **Produces:** connector `hinge.pin` v1.
  - **Params:** `length: float mm` (30), `knuckle_diameter: float mm` (6), `knuckles: integer` (5, odd), `leaf_width: float mm` (10), `leaf_thickness: float mm` (2.4), `pin_diameter: float mm` (2.5), `gap: float mm` (`default_from: fit.hinge_gap`), `open_angle: float deg` (0, the print pose).
  - **Parts:**
    - `leaf_a` (side A, union) carries the odd knuckles and the pin, fused;
    - `leaf_b` (side B, union) carries the even knuckles, bored to `pin_diameter + 2·gap`;
    - axial gaps of `gap` separate the knuckles;
    - 45° cones on the pin ends make the bores printable without support.
- [ ] **Step 1:** Tests:
  - `hinge_leaves_never_touch` (Task 9's detection finds no intersection between `leaf_a` and `leaf_b`, and their minimum distance is `gap` ± the chord tolerance);
  - `pin_radial_gap_measured_on_meshes`;
  - `hinge_rotates_90_degrees_without_collision` (rotate `leaf_b` about the axis in 5° steps and detect intersections at each);
  - `hinge_parts_closed_and_manifold`.
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(parts): print-in-place pin hinge [MF-MODEL-005]`.

### Task 27: Snap clasp

**Files:** `crates/mf-parts/src/connectors/clasp.rs`. Test: `crates/mf-parts/tests/clasp.rs`

**Interfaces:**
- **Produces:** connector `clasp.snap` v1.
  - **Params:** `width: float mm` (12), `arm_length: float mm` (10), `arm_thickness: float mm` (1.6), `lip_depth: float mm` (`default_from: fit.clasp_interference`), `gap: float mm` (`default_from: fit.hinge_gap`).
  - **Parts:**
    - `hook` (side B, the lid; union) is a cantilever arm with a lip;
    - `catch` (side A, the body; union) is a ridge.
  - In the closed print pose, the lip sits `gap` clear of the catch, and overlaps it in projection along the arm's flex direction by exactly `lip_depth`.
- [ ] **Step 1:** Tests:
  - `clasp_parts_dont_touch_in_print_pose`;
  - `lip_overlaps_catch_by_interference_measured_on_meshes`;
  - `arm_strain_at_engagement_under_3_percent` (beam formula from the parameters, reported; the parameters are refused by name above 3%, a PLA-safe limit, with "Make the arm longer or thinner").
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(parts): snap clasp [MF-MODEL-005]`.

### Task 28: `connector.join` and save to library

**Files:** `crates/mf-operations/src/connector_ops.rs`, `specs/operations/connector.join.yaml`, `crates/mf-parts/src/manifest.rs` (`kind: connector`). Test: `crates/mf-operations/tests/connector_join.rs`

**Interfaces:**
- **Consumes:** `mf_boolean::boolean` with D15 local unions (Task 8); `intersections_between` (Task 9).
- **Produces:**
  - `connector.join` v1:
    - the target is the connector object; operands `side_a` and `side_b` (consumed: no; changed);
    - in one command, each `Union` role is unioned into its side, each `Subtract` role is subtracted from its side, and `Separate` roles become new objects;
    - the connector object is removed;
    - afterwards, `side_a` and `side_b` must not intersect, and their minimum distance at the connector must be at least the connector's `gap` minus the chord tolerance. Otherwise the join is refused as `moving_parts_fused` ("The hinge would print fused to the lid here; increase the gap or move the hinge").
  - `PartManifest.kind: Connector { connector: String, params: Params }`. "Save to library" on a connector object writes one, and dropping it from the library regenerates it through `object.add_connector`.
- [ ] **Step 1:** Tests:
  - `join_unions_each_leaf_into_its_side_in_one_command`;
  - `fused_moving_parts_are_refused`;
  - `saved_connector_regenerates_identically`;
  - `connector_join_replays_exact`.
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(operations): connector join and library entries [MF-MODEL-005]`.

### Task 29: Connectors in the desktop

**Files:** `crates/mf-desktop/src/{connector_tool.rs (new), parts_tool.rs, transform_tool.rs, panels.rs}`. Test: `crates/mf-desktop/tests/connectors.rs`

**Behaviour:**
- **The library.** The Parts section gains a **Connectors** group: Pin hinge, Snap clasp, and the user's saved connectors.
- **Placing.** Dragging one onto a model places it on the surface, as with parts. Near a seam between two objects (coplanar cut faces less than `cut_gap + 0.5 mm` apart), a hinge snaps its axis onto the seam line and a clasp centres across it.
- **Transform.** The Transform tool moves and rotates the connector as one object. Its scale handles are replaced by **Size** fields (length, diameter, …) that run `object.edit_connector` (D22).
- **Properties:**
  - "Pin hinge · joins scan (body) ⇄ scan (lid)", with the side pickers prefilled from the seam;
  - "Gap 0.4 mm (from Print fit: Default)";
  - **Merge** (`connector.join`) and **Save to library…**.
- **Splitting a lid.** The Split tool's "Gap between parts" offers "Use hinge gap (0.4 mm)" when the target is meant for a print-in-place lid. It shows as a checkbox, "Print-in-place lid".
- [ ] **Step 1:** GUI tests:
  - `hinge_snaps_to_the_seam`;
  - `size_fields_regenerate_and_keep_the_gap`;
  - `merge_shows_fused_refusal_with_next_action`;
  - `save_connector_to_library_appears_in_connectors`.
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(desktop): connector placement, resize and merge [MF-MODEL-005]`.

### Task 30: UC3 acceptance scenario: hinged box

**Files:** `crates/mf-cli/tests/uc3_hinged_box.rs`, `crates/mf-desktop/tests/uc3_capture.rs`, `scripts/capture-uc3.sh`

- [ ] **Step 1: Scenario test (CLI, end to end)** `uc3_hinged_box`, on `hollow_box(80 × 60 × 40 mm, wall 3 mm)` (closed, with an internal void):
  1. `mesh.split_plane` horizontally 8 mm below the top, with `cut_gap` from the fit profile's `hinge_gap`. This gives "box (top)", the lid, and "box (bottom)", the body.
  2. Two `object.add_connector hinge.pin` on the back wall, centred on the seam 15 mm from each end, then `object.edit_connector length=24`.
  3. One `object.add_connector clasp.snap` on the front wall, centred on the seam.
  4. A `connector.join` for each connector, with side A the body and side B the lid.
  5. Save the 3MF and the log.
  - **Assert:**
    - the document holds exactly two objects, body and lid, each closed and manifold;
    - the body and lid don't intersect anywhere, and their minimum distance is ≥ `hinge_gap` − the chord tolerance;
    - each hinge's pin bore is measured at `gap`;
    - rotating the lid about the hinge axis from 0° to 90° (in 5° steps) collides only at the clasp's lip, which overlaps by `clasp_interference` as designed, and nowhere else;
    - the recorded params hold the fit profile's values;
    - replay is all-Exact.
- [ ] **Step 2: GUI capture** `uc3_capture`: split with "Print-in-place lid", drag two hinges and a clasp from the library, snap to the seam, resize a hinge with the Size fields, Merge each, then a view-only rotation of the lid. Saved to `artifacts/phase1/p1-m6/uc3/`. Stage the print check for the owner (non-blocking) in `artifacts/phase1/p1-m6/print-check/`.
- [ ] **Step 3:** FAIL → fix → PASS.
- [ ] **Step 4:** Commit `test: UC3 hinged-box acceptance scenario`.

**Exit criteria (P1-M6):**
- UC3 passes (Task 30).
- MF-MODEL-005 is met for the pin hinge and the snap clasp. The other connector types (snap-fit hinge, ball-and-socket, dovetail connector) are registered as issues for Phase 2.
- The connector API is plugin-ready (D22).
- The captures are in `artifacts/phase1/p1-m6/`.

---

# Phase 1 close-out

### Task 31: Cross-OS equivalence, the messy corpus and benchmarks

- [ ] Extend the `replay-cross-os` job in `heavy.yml` with a log that exercises every Phase 1 operation (`crates/mf-command/tests/data/phase1-all-ops/`). It must give Equivalent or Exact on all three OSes.
- [ ] The UC1–UC3 scenario tests are in the cross-OS job (Equivalent or Exact on all three OSes).
- [ ] Extend `messy_corpus.rs`: boolean, split and join on every model. Each is valid or a named refusal. Results go to `artifacts/phase1/messy/results.json`.
- [ ] Benchmark JSON with device info for every timing gate above, labelled preliminary (OD-2).
- [ ] Commit `test: Phase 1 cross-OS replay, corpus and benchmarks`.

### Task 32: Independent reviews

- [ ] **Technical whole-branch review.** It challenges shortcuts that pass the tests without serving the architecture. Are operands really reusable by connectors, plugins and scripts? Is the validator really independent of the kernel? Does the window really leave kept faces bitwise unchanged?
- [ ] **UX assessment** from the capture reels. Could a Meshmixer user split, peg, sculpt, meshmix and add hinges without guidance (UC1–UC3)? Does every colour mean one thing? Does every refusal have a next action?
- [ ] Log every deferred finding in `docs/issues.md`.

### Task 33: Owner-run acceptance demo

- [ ] `scripts/demo-phase1.sh` builds the release app and stages the inputs in `artifacts/phase1/demo/`: the real scan, a 30M fixture generated into a temp dir, and a two-object 3MF.
- [ ] `docs/milestones/p1-demo.md`, about 20 minutes. Each step names what to look for and the Meshmixer feature it matches or exceeds:
  1. Move, rotate and drop an object onto the plate (P1-M1).
  2. See two objects intersect, union them, undo, then subtract (P1-M2).
  3. Split the scan with automatic pegs, export 3MF, and open it in a slicer (P1-M3).
  4. Sculpt with symmetry on the 30M fixture (P1-M4).
  5. Meshmix a part onto the scan with a blend (P1-M5), then run Rocktopus (UC2).
  6. Build the hinged box (UC3), and run UC1's split with dowels.
  7. Save the log, quit, and replay it to all-Exact.
- [ ] Before the demo, the owner reviews every deferred checkpoint capture in `artifacts/phase1/` together.
- [ ] The owner runs it. That run is the acceptance gate. Record the outcome in `docs/milestones/p1.md`.

### Task 34: Register the algorithm limits and deferred scope

- [ ] Add issues to `docs/issues.md` (next free IDs, currently 056 onward). Each gets a severity, a milestone and resolution criteria:
  - subtract and intersect on open targets are refused, and joins whose opening is uncovered or too large are refused (the kernel needs closed input, D15);
  - zippering of open stamp parts (D14);
  - drawn-path cuts (D11);
  - the kernel is slow on heavily self-intersecting large inputs (upstream-documented);
  - a dovetail angle parameter;
  - the connector types not in Phase 1 (snap-fit hinge, ball-and-socket, dovetail connector), and connector metadata that 3MF export doesn't keep (D22);
  - pen pressure on platforms where egui doesn't report it (D19);
  - any qualification gate that passed only narrowly.
