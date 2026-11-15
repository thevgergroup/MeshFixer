# ADR-0008: The preview is the operation's own plan

Status: Accepted (consolidation-plan.md, owner approved W0) · 2026-11-14

## Context
Desktop previews re-implement op geometry, some on the UI thread. Split's region preview cuts on every drag frame, and a Hollow click scans every edge, which freezes at 5–30M tris. Under non-uniform scale, Split's preview plane differs from Apply's. (architecture-review.md §1, §3)

## Decision
- Every op that previews exposes `plan(input, params, ctx) -> Plan`, and `execute` consumes that same plan. The desktop renders plans and never re-implements op geometry.
- Plans run on the preview lane, keyed by `(params, base revision)`. Nothing O(n) runs on the UI thread.

## Enforcement
A `preview_equals_apply` property per op; the 5M frame gate; and removing mf-desktop's dependency on mf-boolean, so the compiler enforces it.
