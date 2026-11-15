# ADR-0009: One job system with declared semantics

Status: Accepted (consolidation-plan.md, owner approved W0) · 2026-11-14

## Context
The command queue merges any two queued commands with the same op and object, so rapid sculpt strokes or hole fills can be dropped. Workers, threads and follow-ups (`pending_*` fields) are ad hoc per tool. (architecture-review.md §3)

## Decision
- There are three lanes: a serial command queue, `PreviewSlot`s and the pick/hover lane. Each is panic-safe and cancellable.
- No `std::thread::spawn` and no `RunContext::none()` in tools.
- Coalescing is declared per spec as `coalesce: absolute | never`, defaulting to `never`.
- A follow-up is a continuation on its invocation, not a `pending_*` field.

## Enforcement
`every_spec_declares_coalescing`, `rapid_strokes_are_all_recorded`, and a thread-spawn lint.
