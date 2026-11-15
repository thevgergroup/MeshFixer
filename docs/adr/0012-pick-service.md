# ADR-0012: Picking and regions are one service

Status: Accepted (consolidation-plan.md, owner approved W0) · 2026-11-14

## Context
Surface picking, region growth, cap and joint detection and hover are re-implemented per tool. Some pick against the stored transform rather than the shown one, and some run O(n) on the UI thread. (architecture-review.md §2)

## Decision
- `PickService::surface(px)` is the only ray builder. It always picks against the shown transform, and runs on the pick lane above a size threshold.
- Region growth (`Crease | Tag | Component`) is a `SelectionKind`, resolved on the cached `Topology`.

## Enforcement
A `cam.ray(` lint outside `pick.rs`, plus the 5M click gate.
