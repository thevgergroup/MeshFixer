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
