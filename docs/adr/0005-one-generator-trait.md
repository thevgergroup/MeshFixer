# ADR-0005: One Generator trait for shapes and connectors

Status: Accepted (parts-architecture-review.md, owner 2026-11-12) · 2026-11-12

## Context

Shapes (`Shape`, one mesh) and connectors (`Connector`, N parts with a
side and a join kind) were two traits, with three registries
(`connectors()`, `JOINT_CONNECTORS`, the hand-listed library entries)
and per-connector `match id` blocks in Connect's panel, preview,
placement and in `split/joints.rs`'s checks and fixes. Adding one
connector meant 8–10 edits across 4 files in 2 crates.

## Decision

- **`mf_parts::Generator`** is the only generator trait: a param schema
  (`spec()`), `generate(params, &GenContext) -> Vec<ShapePart { name,
  role: Solid|Hole, attach_to: A|B|Neither, mesh }>`, `checks()` returning
  data (`GenCheck { id, message, fix: Fix { label, patch: ParamPatch } }`)
  and `placement_hint()` (Surface, Point, DragLine, Manual). A plain shape
  is the one-part, `Neither` case. `GenContext` carries the run and,
  when placed, a `Targets` query over the objects it attaches to.
- Everything a consumer used to branch on is a trait fact: the fit
  probe and its label, the anchor (joint-wide anti-rotation), whether
  holes are breakout-checked, the drag handles' params, quick params
  shown before placing, a hinge's moving gap, the params a dragged line
  sets, and `upgrade_params` for records from an older schema.
- **One registry**, `mf_parts::generators()` / `find_generator`; the
  library's connector entries and Connect's kinds are derived from it.
- **One apply path**, `mf_operations::apply_parts`: `mesh.combine`,
  `object.attach` (now also building generators in place), the legacy
  `connector.join` (kept only so old logs replay, byte-identical) and
  `split.place_connectors` all fold parts into targets there.
- **Checks**: `split/joints.rs` keeps only the generic checks (off the
  cut face, edge distance, breakout, fit), by id; each fix is a param
  patch (or, for position, a moved placement).
- **UI and preview from the data**: one schema-driven field grid
  (`shape_panel::param_grid`) for the selection panel, Connect and the
  Connectors library; a placement's footprint is the section of its own
  generated parts at its cap.

## Consequences

- A new connector is its generator file, its spec YAML and one registry
  entry; `test.key` (the `test-kinds` feature) proves it end to end.
- Schema types now carry UI meaning: the dowel/peg cross-section is a
  `choice`, the dovetail's open ends are `bool`s. Older numeric records
  upgrade through `upgrade_params` (placements keep recording numbers:
  a choice as its index, a bool as 1/0).
- A footprint is computed by generating the parts (cheap prisms today);
  a heavy generator would want its outline cached.
