# ADR-0004: Mixing join

Status: Accepted (Phase 1 plan, D14 and D15) · 2026-11-10

## Context

Meshmixer's signature tool ("Meshmix") drags a part onto a model and
joins it: solid parts are unioned, open "stamp" parts are zippered along
their boundary and blended into the surface. P1-M5 (MF-MODEL-004) has to
match that for solid parts and go further — a guaranteed-valid join
(D4), with the blend's width recorded and the result validated — without
building a second, zippering kernel.

This decision is about *how a join is defined* (boolean union plus an
optional blend, and what happens to open targets and open parts). It
does not depend on which boolean kernel implements the union: that
question is ADR-0002's, and is currently **NO-GO** pending
re-qualification (Phase 1 Task 7b). The requirements below — D4's
independent validator and D5's local window — are kernel-independent by
construction: the validator runs on the kernel's output whichever kernel
produces it, and the window clips the input before any kernel call.

## Decision

**Mixing joins by boolean union plus an optional blend. Zippering an
open boundary is not built in Phase 1** (D14).

- **The blend.** When `blend_width > 0`, the seam band (every vertex
  within `blend_width` edge-path distance of the union's seam) is
  remeshed to the seam's mean edge length (MF-SCULPT-002's local
  remesher) and then faired (Δ² with two fixed rings outside the band),
  then validated by the independent validator (MF-P1-VAL-001, D4). Union
  only; other operations with `blend_width > 0` are refused by name.
- **Open "stamp" parts** (`PartKind::Open`) are capped automatically
  (`fill_holes` flat) and sunk by `embed_depth` along the attach normal
  before the union, so the blend hides the cap. One robust path serves
  both closed and open parts; the cap's faces never survive the union.
- **Rejected: zippering.** Projecting a curve onto the surface, cutting
  the target along it and bridging is a second fragile kernel with no
  manifold guarantee. If blends on thin, open stamp parts look worse
  than Meshmixer's, zippering is deferred to Phase 2 (tracked in Task
  34).

**Open targets: joins use local-window closure; subtract and intersect
need a closed target** (D15, amended 2026-11-10 for mixing and UC2).

- **Subtract and intersect** are refused on an open target, because
  "inside" isn't defined for it. The refusal names the reason
  (`operand_not_closed`) and offers the one-click next action "Fill all
  holes, then combine" (`fill_holes` then the operation, as two
  undoable commands) — MF-P1-VAL-001's no-silent-fallback rule applies
  here too: nothing is filled without the user's say.
- **Unions are local** (joins, mixing, connector leaves), scaling by the
  same local window as every other boolean (MF-P1-SCALE-001, D5):
  - Holes outside the window don't matter; those faces are kept as they
    are, bitwise.
  - A hole whose loop lies wholly inside the window is closed
    temporarily for the kernel (`fill_holes` flat, faces tagged
    temporary) — UC2's example is the octopus neck after its cut cap is
    deleted.
  - After the union, every temporary face must be gone, since the tool
    must cover the opening. One surviving is refused as
    `opening_not_covered`, with the next action "move it to cover the
    opening, or fill the hole first." An unrequested fill is never left
    behind.
  - A hole crossing the window's boundary grows the window to contain
    its loop, up to 4x the tool's bounds; beyond that, `opening_too_large_for_join`
    with the same fill-first next action.
  - The result welds back into the open scan by exact vertex identity
    (as D5's weld). Its other holes stay open and stay listed.
- **Rejected:**
  - refusing every open target (the first version of D15): fails UC2 and
    Meshmixer parity on scans;
  - filling the user's holes silently: violates the owner's no-fallback
    rule.

## Kernel independence

Nothing in this ADR is written against `manifold-rust`'s API or
behaviour. The two requirements the Phase 1 spec numbers from these
decisions are kernel-independent:
- **MF-P1-VAL-001** (D4): every modelling result — a plain union, a
  blended union, a local-window closure — is validated independently by
  `mf_geometry::validate_solid`, and an invalid result is refused by
  name. The validator doesn't call into the kernel; it checks the
  kernel's output.
- **MF-P1-SCALE-001** (D5): the window is built before the kernel runs
  (plane clipping, temporary caps) and torn down after it returns
  (provenance-based removal, exact-identity weld). Its cost follows the
  overlap, not the model, regardless of which kernel fills in the middle
  step.

Kernel qualification is **pending re-qualification** (ADR-0002 recorded
`manifold-rust` 0.19.2 as NO-GO on 2026-11-10; Task 7b re-qualifies a
fix or an alternative under the same `Kernel` trait and the same
`qualify.rs` gates). P1-M2 Tasks 8–10 and this join design are blocked on
that outcome for *execution*, but not for the join's rules, the
validator's contract, or the window's shape, which this ADR fixes now so
Task 2's spec and schema work can proceed independently of Task 7b.

## Alternatives considered

- **A boolean as a half-space box for local closure.** Rejected (carried
  over from D11/D5's reasoning): whole-mesh cost on a tilted or
  off-centre opening defeats the local window.
- **Always filling every hole before a union, whether or not it's in the
  window.** Rejected: it would silently change geometry the user didn't
  ask to touch, and holes outside the window are explicitly kept as they
  are (D5).

## Consequences

- The refusal copy table (`operand_not_closed`, `opening_not_covered`,
  `opening_too_large_for_join`, plus `blend_self_intersects` and
  `blend_did_not_converge`) is fixed now, so the desktop's Properties
  panel copy (Task 10, Task 23) and the CLI's error text can be written
  against stable names before P1-M2/M5's implementation lands.
- If temporary closure upsets the kernel near thin openings (whichever
  kernel passes Task 7b), those joins are refused by name; the
  refusal's frequency on the messy corpus is reported and kept as an
  issue if it's above 5% (carried over from D15's cost-if-wrong note).
- `phase1-spec.md` records MF-P1-VAL-001 and MF-P1-SCALE-001 as
  kernel-independent requirements; it does not assert a kernel decision,
  which stays ADR-0002's.
