# MeshFixer Phase 1 Engineering Spec (v0.1)

v0.1 (2026-11-10, Phase 1 Task 2): turns the Phase 1 plan's decisions
(D1–D24, `docs/superpowers/plans/2026-11-10-phase1-modelling-tools.md`)
into numbered, testable requirements. Executors read this spec together
with `docs/spec/phase0-spec.md` §11a/§11b, which named the Phase 1
requirements before this plan existed; the IDs there (MF-OBJ-001,
MF-MODEL-001…005, MF-SCULPT-001…005) are unchanged and this spec doesn't
renumber them. This spec adds the contract-level requirements the plan's
decisions introduce (operands, object changes, the validator, the
window) and the owner's three acceptance use cases.

Status: Approved for implementation · 2026-11-10
Licence: MIT OR Apache-2.0 · Dev machine: Apple M2 Max, 32 GB, macOS

## Purpose

Phase 0 built one target, one operation at a time. Phase 1 adds tools
that touch more than one object (a boolean's tool, a split's new part, a
placed library part) and tools that move things (the gizmo, parts,
connectors). This spec is kernel-independent throughout: the boolean
kernel question is ADR-0002's (currently NO-GO, pending re-qualification
in Task 7b) and is never referenced by requirement number here. Every
requirement below holds whichever kernel eventually passes Task 7b's
gates, because the validator and the window are our own code around the
kernel, not the kernel itself.

## 1. The command contract (D6, D7)

**MF-P1-CMD-001: Operands and object changes.** A command may declare
*operands* beyond its target: other objects or selections it reads,
consumes or keeps.
- A spec's `operands:` list is optional (`#[serde(default)]`); a spec
  that doesn't declare any operands behaves exactly as in Phase 0, and
  every spec YAML written before Phase 1 validates unchanged against
  `specs/operation.schema.json`.
- Each entry is `{ name, role: read | consumed | kept, kinds: [object,
  face_set, component], required }`.
  - `read`: read for geometry or provenance, never changed or removed.
  - `consumed`: removed once the command commits (for example a
    boolean's tool), unless the operation's own parameters say
    otherwise (for example `keep_tool: true`, which makes it `kept`).
  - `kept`: read, and explicitly not removed.
- An operation's result is a set of `ObjectChange`s per object: geometry
  changed, transform changed, object created, or object removed. A
  command's verdict (for replay) is the worst verdict among its
  objects.
- Connectors (MF-MODEL-005), plugins (MF-PLUG-001) and scripts
  (MF-SCRIPT-001) reuse this same contract; none gets a parallel,
  spec-bypassing path (carried over from Phase 0's MF-SPEC-003).

*Accept:* `operands_section_validates_against_schema` (`crates/mf-spec`)
parses an operand-bearing spec against the schema with no errors, and
confirms every built-in spec validates against it, with every spec
written before Phase 1 (the `mesh.*` operations) still parsing with an
empty operands list. Later specs may declare operands (the test-only
`test.consume_tool`, Task 3). `vec3_and_affine_params_validate_and_round_trip` confirms
the two new parameter kinds this contract needs (next section)
round-trip through a command log. Task 3's `operand_provenance_is_total`
and `undo_boolean_restores_the_tool_with_its_id` extend this to the
command and document layers once operations use it.

**MF-P1-SEL-001: Operand provenance.** Provenance is total over every
operand, not just the target (extends Phase 0's MF-SEL-002).
- `Sources` gains `operands: Vec<(u32, FaceId)>`: a created face may
  name a face of any operand, by its operand index, with
  `#[serde(default)]` so Phase 0 provenance records still deserialize.
- A command's `CommandRecord` gains `operands` (per named operand: the
  object, its selection if any, and the input revision/structure
  hash/transform it was read at) and `outputs` (per changed or created
  object: hash, structure hash, transform), both `#[serde(default)]`.
- The log format moves to `mfx-log/2`; `mfx-log/1` logs still replay
  (Task 3's `mfx_log_1_still_replays`).

*Accept (Task 3, once `mf-operations`/`mf-command` adopt the contract):*
`operand_provenance_is_total` — a property test — confirms every output
face of every changed or created object is accounted for exactly once,
and every face of a consumed operand is either a source or listed
removed.

**D7 parameter types.** Two new `ParamKind`/`ParamValue` kinds carry a
point/direction and an absolute transform, so a transform is recorded
as the resulting matrix rather than as deltas that platform `libm`
could make drift (D7's rationale):
- `ParamKind::Vec3 { default: [f64; 3] }` / `ParamValue::Vec3([f64;
  3])`: a point or direction, in document units when `unit: mm` is set
  (next section), unitless otherwise (a direction).
- `ParamKind::Affine { default: [f64; 12] }` / `ParamValue::Affine([f64;
  12])`: the 12 values of a row-major 3x4 matrix (3x3 linear part, then
  translation) — the resulting transform, not a delta.
- Both serialize as JSON arrays in a command log, and as comma-separated
  values on the CLI (`--param transform=1,0,0,0,1,0,0,0,1,10,20,30`) and
  in spec YAML (`default: [0.0, 0.0, 0.0]`).

*Accept:* `vec3_and_affine_params_validate_and_round_trip`
(`crates/mf-spec/tests/operands_and_params.rs`): a spec with `vec3` and
`affine` parameters parses its YAML default, parses CLI-style
comma-separated text, rejects a wrong element count by name (never
truncating or panicking), and round-trips through `serde_json` bit for
bit.

## 2. Units (D20, Global Constraints)

**Printing parameters are in document units.** A parameter's default is
written in millimetres in its spec (`unit: mm`); the value actually used
is converted to the document's units and that converted value is what's
recorded, so replay never re-reads a unit or re-derives a conversion.

`OperationSpec::to_document_units(&mut Params, unit: Unit)` converts
every parameter marked `unit: mm`:
- a `float` parameter's number is scaled;
- a `vec3` parameter's three components are scaled (a length vec3, such
  as a plane point or a hole centre — a *direction* vec3 carries no
  `unit` and is left alone);
- every other parameter kind is untouched (none is ever marked `unit:
  mm`).

This runs once, in `prepare`, before the parameters are recorded in the
command. `Unit` is `mf_io::threemf::Unit` (already the document's own
unit type, re-exported by `mf-document` and used by `mf-command`), so
`mf-spec` depends on `mf-io` for it; `mf-io` has no dependency on
`mf-spec`, so this introduces no cycle.

*Accept:* `lengths_convert_to_document_units`
(`crates/mf-spec/tests/operands_and_params.rs`): a float and a vec3
parameter marked `unit: mm` convert correctly for a metre-scale document
(Review Focus 4's example shape: 1 mm becomes 0.001 document units), a
millimetre document converts by exactly 1.0, and parameters without
`unit: mm` (an integer count, a unitless direction vec3) are untouched.
Task 13's `default_clearance_is_0_2_mm_in_a_metre_document` extends this
to a real operation (alignment holes) once Task 13 lands.

## 3. Validation and scale (kernel-independent)

These two requirements are stated without reference to which boolean
kernel implements the union/subtract/intersect step. ADR-0002 recorded
`manifold-rust` 0.19.2 as **NO-GO** (2026-11-10); kernel qualification
is **pending re-qualification** under the same `mf-boolean::Kernel`
trait and the same `qualify.rs` gates (Task 7b). Neither requirement
below depends on that outcome: the validator runs on whatever a kernel
returns, and the window is built and torn down by our own code before
and after any kernel call.

**MF-P1-VAL-001: Independent validation, no silent retry (D4).** Every
modelling result is validated independently, and invalid output is
refused by name. There is no automatic retry with another method.
- `mf_geometry::validate_solid` checks the output is closed, manifold
  (edges and vertices), consistently oriented, has positive volume, and
  has no degenerate faces. It also checks that no face created by the
  operation intersects any other face (exact predicates).
- Any failure is an `OpError` with a code, a plain-language message and
  the faces involved. Nothing commits on failure (consistent with
  Phase 0's MF-CMD-006: failure leaves the document unchanged).
- Trusting the kernel, or silently falling back to a slower method, is
  rejected (the owner's no-fallback rule, carried from the Best
  Practices constraint of not hiding errors behind a fallback).

*Accept (Task 8, once `mf-boolean`/`mf-geometry::validate_solid` exist):*
`validator_catches_an_injected_self_intersection` — a fault-injected
kernel returns a bad mesh, and the operation refuses it by name rather
than committing. The validator's own unit tests cover each
`SolidDefect` variant (`Open`, `NonManifoldEdge`, `NonManifoldVertex`,
`Inconsistent`, `Degenerate`, `SelfIntersects`, `NonPositiveVolume`)
independently of any kernel.

**MF-P1-SCALE-001: Local-window scaling (D5).** A modelling result costs
what the overlap between the operands costs, not what the whole model
costs.
- Before any kernel call, the target is clipped against an axis-aligned
  box: the tool's bounds, grown by a margin. Classification against an
  axis-aligned plane is exact in `f64` (a coordinate compared with a
  constant), and constructed cut points lie exactly on the plane.
- The clipped part is capped into a closed mesh (constrained Delaunay
  triangulation, D3) and sent to the kernel with the tool. The temporary
  caps are removed by provenance afterward, and the result is welded
  back by exact vertex identity.
- Faces outside the window are `Kept`, bitwise and in order (Phase 0's
  MF-OP-003, extended to multi-object commands by this spec's
  MF-P1-CMD-001).
- Whole-mesh kernel calls are rejected: a 30M-triangle exact arrangement
  is neither interactive nor memory-safe (D5's rationale).

*Accept (Task 8):* `faces_outside_the_window_are_kept_bitwise` confirms
untouched faces are bit-identical and in the same order through a
boolean on a large target with a small tool. The window's clip/cap/weld
steps (`clip_to_box`, `triangulate_section`, the weld in
`mf-boolean/src/weld.rs`) have their own unit tests that never construct
or depend on a `Kernel` implementation.

## 4. Performance targets by milestone

These are the interactive-scale targets each milestone's exit criteria
measure (Global Constraints: "interactive at 5–30M"; M2 Max results are
preliminary, OD-2 from `phase0-spec.md`).

| Milestone | Requirement | Target |
|---|---|---|
| P1-M1 (MF-OBJ-001) | Drag frame time at 5M, 10M, 30M | within orbit frame time + 1 ms (p95) |
| P1-M1 | A transform commit | < 1 ms on the UI thread |
| P1-M2 (MF-MODEL-001) | 1M x 1M union | ≤ 10 s |
| P1-M2 | 30M scan x 50k part union | ≤ 3 s |
| P1-M2 | Cancel-to-stop, anywhere including inside the kernel | ≤ 250 ms |
| P1-M3 (MF-MODEL-003) | 30M split | ≤ 10 s |
| P1-M4 (MF-SCULPT-001…004) | Dab-to-screen at 5M | ≥ 60 fps |
| P1-M4 | Dab-to-screen at 30M | ≥ 30 fps |
| P1-M4 | 1,000-stroke history memory | ≤ 1.5x one revision + footprints |
| P1-M5 (MF-MODEL-004) | 30M scan x 50k part merge | ≤ 3 s |
| P1-M5 | Blend | ≤ 2 s |
| P1-M6 (MF-MODEL-005) | — | no new timing gate; reuses P1-M2's local-union cost |

Every milestone also measures 30M, per the Global Constraints' binding
goal of interactive at 5–30M triangles; M2 Max numbers are preliminary
until a lower-spec reference machine is available (OD-2).

## 5. Owner use cases as acceptance requirements (UC1–UC3)

Each use case is an automated scenario (CLI or commands) plus a GUI
frame capture, gating the milestone named in the Phase 1 plan's table.
The scenario assertions below are copied from the tasks that implement
them (Tasks 16a, 24a, 30); this spec numbers them as requirements so the
acceptance bar is fixed before those tasks are built.

**MF-P1-UC-001: UC1, split and pegs (completed in P1-M3, Task 16a).**
Split a model horizontally, vertically and on a bias; add alignment
holes on both cut faces; dowels are generated as separate objects, laid
out on the plate, sized from the fit profile.

Scenario `uc1_split_horizontal_vertical_bias_with_dowels`, on the
procedural 60 mm sphere and on the real scan (closed with `fill_holes`
first): `mesh.split_plane` three times (horizontal, vertical, 30° bias),
each with `--param pegs=dowels hole_depth_above=5 hole_depth_below=8`
and `--fit-profile Default`, then export 3MF.

*Accept:*
- every part and dowel is closed and manifold;
- dowel length = 5 + 8 - 0.4 mm and diameter = hole - 0.2 mm, measured on
  the meshes;
- the dowels are separate 3MF objects with world min z = 0, not
  overlapping any object's footprint;
- the recorded params hold the profile's values;
- `meshfixer replay` gives all-Exact with a different active profile.

**MF-P1-UC-002: UC2, "Rocktopus" (completed in P1-M5, Task 24a).** Cut
the head off an octopus and delete it. Bring in a new head, place it on
the neck with the gizmo, join with a blended union, and touch up with
sculpt brushes. Works whether the neck is capped or left open.

Scenario `uc2_rocktopus`, run twice (`capped_neck` and `open_neck`):
import the octopus; `mesh.split_plane` through the neck (`keep: both`);
`object.delete` the head piece; (open-neck variant only)
`mesh.delete_faces` on the body's `cap_faces`, leaving the neck open;
import the head; `object.set_transform` with `compose(scale 1.3, rotate
(0, 0, 25°), move onto the neck)` as the gizmo would produce, base
overlapping the neck by 2 mm; `mesh.boolean` union with `blend_width = 3
mm`; two `mesh.sculpt_stroke`s (smooth, then inflate) along the seam.

*Accept:*
- the document has exactly one object, closed and manifold, passing
  `validate_solid` (no self-intersections);
- the seam band is faired: band vertices satisfy the discrete Δ²
  equations to 1e-8 relative (before the strokes), and the largest
  dihedral angle across the band is below the sharp, unblended union's
  on the same inputs;
- nothing outside the blend window changes: every octopus face outside
  the window is `Kept` and bitwise equal through the join and both
  strokes (`untouched_outside_target`);
- the open-neck variant gives a closed result through local-window
  closure (this spec's MF-P1-SCALE-001, extended by D15/ADR-0004), with
  no temporary face left;
- a variant with the head moved 10 mm off the neck is refused as
  `opening_not_covered`, and the document is unchanged;
- the saved log replays all-Exact.

**MF-P1-UC-003: UC3, hinged box (completed in P1-M6, Task 30).** Split
the lid off a hollow box. Add two print-in-place hinges and a snap
clasp from the library, resize and place them, and merge each leaf onto
body or lid.

Scenario `uc3_hinged_box`, on `hollow_box(80 x 60 x 40 mm, wall 3 mm)`:
`mesh.split_plane` horizontally 8 mm below the top, `cut_gap` from the
fit profile's `hinge_gap`, giving "box (top)" (lid) and "box (bottom)"
(body); two `object.add_connector hinge.pin` on the back wall, centred
on the seam 15 mm from each end, then `object.edit_connector length=24`;
one `object.add_connector clasp.snap` on the front wall, centred on the
seam; a `connector.join` for each connector (side A body, side B lid);
save the 3MF and the log.

*Accept:*
- the document holds exactly two objects, body and lid, each closed and
  manifold;
- the body and lid don't intersect anywhere, and their minimum distance
  is ≥ `hinge_gap` minus the chord tolerance;
- each hinge's pin bore is measured at `gap`;
- rotating the lid about the hinge axis from 0° to 90° (5° steps)
  collides only at the clasp's lip, which overlaps by
  `clasp_interference` as designed, and nowhere else;
- the recorded params hold the fit profile's values;
- replay is all-Exact.

## 6. Decision cross-reference

For traceability, this table maps the plan's decisions (D1–D24) to the
requirement or document that carries them. A decision not yet
implemented stays a decision until its task lands; this spec doesn't
assert it has.

| Decision | Carried by |
|---|---|
| D1 (boolean kernel) | ADR-0002 (NO-GO, pending ADR-0002b / Task 7b); not referenced by number elsewhere in this spec |
| D2 (exact predicates) | MF-P1-VAL-001 |
| D3 (cut-cap triangulation) | MF-P1-SCALE-001 |
| D4 (independent validation) | MF-P1-VAL-001 |
| D5 (local window) | MF-P1-SCALE-001 |
| D6 (multi-object contract) | MF-P1-CMD-001, ADR-0003 |
| D7 (absolute transform, vec3/affine) | MF-P1-CMD-001 §1, ADR-0003 |
| D8, D9 (gizmo colours, tokens) | `docs/design/visual-language.md` (Task 5) |
| D10 (boolean frame, consumed tool) | MF-P1-CMD-001 (operand roles); Task 8 |
| D11 (plane cut) | MF-MODEL-003 (`phase0-spec.md` §11b); Task 12 |
| D12 (pegs and dowels) | MF-MODEL-003; Task 13 |
| D13 (sculpting before mixing) | milestone order (P1-M4 before P1-M5) |
| D14 (mixing join: union + blend) | ADR-0004 |
| D15 (open targets, local closure) | ADR-0004; MF-P1-UC-002's open-neck variant |
| D16 (parts library) | MF-MODEL-004; Task 21 |
| D17 (BVH moves to mf-geometry) | Task 8 |
| D18 (new crates) | workspace `Cargo.toml`; `mf-boolean`, `mf-parts` |
| D19 (pen pressure) | MF-SCULPT-001 (`phase0-spec.md` §11a) |
| D20 (print fit profile) | §2 above (units); Task 12a |
| D21 (plate layout) | Task 13, Task 21 |
| D22 (parametric connectors) | MF-MODEL-005; Task 25 |
| D23 (`cut_gap`) | MF-MODEL-003; Task 12 |
| D24 (procedural fixtures) | `mf-fixtures`; Task 24a |
