# ADR-0011: Exact decisions, tagged constructions

Status: Accepted (consolidation-plan.md, owner approved W0) · 2026-11-14

## Context
The exact predicates are private to mf-geometry, so mf-operations and mf-parts decide "on plane" and "same point" with tolerances. Cap detection uses 1e-6, and the e182cb7 window nudge can grow the "local" boolean window to about 134× the box. (architecture-review.md §4)

## Decision
- The public `mf_geometry::exact` is the only place that decides "on plane", "same point" or "inside". Tolerance literals live in one reviewed allow-list.
- Cut caps and rims are tagged per face when built, and joints are derived from the command log.
- The boolean window refuses with `window_degenerate` past the D5 margin.
- There is one affine module.

## Enforcement
The epsilon-literal lint, `affine_is_implemented_once` and `window_never_exceeds_d5_margin`.

## Implementation (C20, 2026-11-15)
- `mf_geometry::exact` is public: `orient2d`, `orient3d`, `side_of`, `on_plane` (a plane given by three points), `same_point` and `point_in_solid` (exact crossing parity).
- `clip_to_box` takes the D5 margin (`ClipOptions::margin`, the margin the window already has over the tool). Its nudges never move a plane past it; past it the boolean refuses with `window_degenerate`. The e182cb7 doubling nudge had reached the whole model on the owner's demo. Enforced by `window_never_exceeds_d5_margin` (mf-boolean `tests/window.rs`) and a `debug_assert` in the clip.
- The demo's crowding came from the split, not the window. The equator ring of a sphere sits 1.2e-15 off the cut plane, so the cut points on its edges landed an ulp from each ring vertex, and the rim carried vertices an ulp apart. `plane_cut` now treats such a vertex as on the plane: on an axis-aligned plane, the cut point is within an ulp of it in every coordinate the edge moves in. This is an f64 comparison, not a tolerance.
- The tolerance allow-list is `docs/tolerances.allow`, enforced by `crates/mf-geometry/tests/epsilon_lint.rs`. It is seeded with the 89 sites found at C20 and only shrinks.
- `mf-command/tests/combine_robust.rs`: under random small transforms, Combine commits a valid solid or refuses with a worded code.
