# ADR-0006: Units, scale and precision. One rule for the whole app.

Status: Accepted (owner request) · 2026-11-14. Amended by consolidation-plan.md: owner crate is `mf-units`.

## Context
We fixed recurring bugs tool by tool, playing whack-a-mole:
- lengths converted twice from mm to document units;
- a fixed 1-unit snap that stopped a gizmo moving on a 0.15 m model;
- an import prompt that just lists units;
- messages showing "at least 0.000".

Each one came from the lack of a single rule for units, scale and precision.

## Decision
1. **Lengths carry their unit.** Every user-facing length is a `Length { value, unit }`. Tool code never passes a bare `f64` as a length. Length params declare `unit` in their spec. Panels build length fields from the spec, through a shared `length_field()` widget.
1b. **Length fields accept units and expressions, as in Fusion and Onshape** (owner-approved 2026-11-14).
- Every `length_field()` parses typed input. Examples:
  - `10 in`, `25.4mm`, `5 cm + 2 mm`, `0.25"`, `1/8 in`;
  - simple arithmetic `+ - * /` and parentheses;
  - a bare number, which means the document's display unit.
- The result is converted to a `Length`.
- Invalid input shows an inline error and keeps the previous value. It is never silently replaced with 0.
- After Enter, the field shows the value in the document's display unit.
- The parser is shared by every length field. It is tested on the units mm, cm, m, in, ft, `"` and `'`, on precedence, and on errors.

2. **Units are converted at exactly one boundary.** The shell, meaning the invocation builder, converts display units to the param's spec unit. `Session::prepare` then converts the spec unit to document units. No panel converts on its own. This replaces the per-panel conversion added in 75514b9.
3. **Scale-relative defaults; no absolute constants.**
   - Snap steps are zoom- and unit-adaptive: a 1-2-5 sequence in mm, where the smallest step is at least about 8 pt on screen.
   - Grid and cell sizes derive from the bounding box.
   - Gizmo and handle sizes are in screen points.
   - Clearances come from the fit profile.
   - Geometric "same point" decisions use exact predicates, not epsilons.

   A literal length constant in document units is a bug.
4. **One formatter.** `format_length` is used everywhere: fields, status line, refusals, History, Inspect. It shows 3 significant figures plus the unit, picks a sensible display unit, and never shows "0.000".
5. **A scale-matrix test.** The core workflows run automatically across:
   - a tiny model in metres (the Stanford bunny, 0.156 m);
   - a normal model in mm;
   - a large model in inches;
   - a 3 m model;
   - zoom extremes, very close and very far.

   Each tool must pass every variant.
6. **Enforcement.** A test or lint fails when:
   - a length param in a spec has no `unit`;
   - tool code calls the raw param builder for a length instead of `length_field`/`Length`;
   - length constants appear in tool or snap code. Keep an allow-list for a reviewed few, if any.

## Consequences
- Units bugs become test failures in one place instead of field reports.
- Every tool panel needs a one-time migration.

## Known violations to fix in the consolidation pass (owner reports)
- **Sculpt radius.** The default "5" became 5 m on the metres bunny, so one stroke moved 34,978 points (the whole model).
  - Make brush size screen-relative, as in Meshmixer: a size in pt under the cursor.
  - Show the real size in the panel ("≈ 4.2 mm at this zoom").
  - Accept typed units.
  - Strength and falloff stay as they are.
- **Gizmo snap.** It was fixed at 1 unit (1 m), so the handle couldn't move. The snap agent is fixing this with a shared `snap_step`. Verify it.
- **Split plane centring.** The plane didn't re-centre after the object was transformed (also with the snap agent).
- **Import units.** Still to do: the Inspect "Reinterpret" / "Scale" control, and remembering choices per folder and exporter (descoped from 4cbbf05).
- **Snap toggle.** A visible Snap toggle in every gizmo tool (with the snap agent).
- **Default audit.** Check EVERY other tool default for absolute constants: Make solid cell, Hollow wall/detail, Split gap, connector sizes, Align, Parts size, the view's near and far planes, grid spacing and plate size.

## Amendment (consolidation plan, 2026-11-14): owner crate `mf-units`
- `Unit`, `Length`, `DocLength`, `parse_length`, `format_length`, `nice_step`, `ScaleCtx` live in a new leaf crate `mf-units` with no mf-* dependencies (mf-spec depends on mf-io and mf-render cannot depend on mf-spec, so neither can own it). `mf-io` keeps `pub use mf_units::Unit` and the 3MF attribute mapping.
- One conversion boundary: the shell's `Intent` builder writes `"<value> <symbol>"`; `Session::prepare` converts to document units. Ops receive doc units plus `DocContext { unit }` and use it to scale mm generators (library parts, connectors).
- Specs declare a typed `ParamUnit { Length(Unit) | Document | Angle | Percent | None }`; a bare `"mm"` string is no longer the trigger.
- Evidence: units-audit.md (89 violations; silent 1000× bugs in library parts, save-as-part, STL/OBJ export, second import).
- Enforced by the spec lint, the desktop conversion-scan lint and the scale matrix.
