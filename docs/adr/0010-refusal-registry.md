# ADR-0010: Refusals are a declared registry

Status: Accepted (consolidation-plan.md, owner approved W0) · 2026-11-14

## Context
About 150 refusal codes are unregistered, and only 12 have a next action. Messages go out through five channels, and Hollow, Align, Sculpt and Repair Apply refusals land in the generic error bar instead of the tool. (architecture-review.md §5)

## Decision
- Each spec lists its refusal codes. Each code has an i18n message id, typed args (lengths as `Length`) and a next action from a closed set.
- Ops return `OpError::Refused { code, args }`. One `RefusalCard` shows it in the panel of the session that issued the work, for previews and Apply alike.
- Info, warnings and refusals share one `Feedback` queue.

## Enforcement
`every_refusal_code_is_declared` and `every_declared_refusal_has_a_message_and_next`, plus a lint that no `OpError::Failed` is constructed outside the registry helper.
