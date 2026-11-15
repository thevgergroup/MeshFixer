# ADR-0007: Tool sessions own their work; the shell owns the lifecycle

Status: Accepted (consolidation-plan.md, owner approved W0) · 2026-11-14

## Context
`shell::Tool` covers only the chrome. Each tool wires its own preview, apply, post-commit follow-up, undo and drag cancel through hand-maintained lists in a 7,179-line `app.rs`. Esc cancels only Transform drags, and Split's joints live in UI state, so undo doesn't restore them. (architecture-review.md §1)

## Decision
- Every tool is a `ToolSession` with `intent`, `preview`, `on_result`, `on_revision`, `cancel_drag` and `paint`. Its state lives in the session, not on `App`.
- Only the shell derives the shown transform and render, the Esc order, undo notification and refusal routing.
- `tools::Interactive` and `tools::CATALOGUE` are deleted.

## Enforcement
`every_tool_honours_the_session_contract` over `shell.tools()`, plus a lint that no tool module is named in `app.rs`.
