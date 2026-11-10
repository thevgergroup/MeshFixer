# ADR-0003: Multi-object commands

Status: Accepted (Phase 1 plan, D6 and D7) · 2026-11-10

## Context

Phase 1's tools all touch more than the command's target: a boolean needs
a tool object, a split creates a new part, a part placement creates an
object from a library mesh, and a transform commits an absolute matrix.
M4's command contract (MF-SPEC-001…003) only names one target per
command, and has no parameter type for a point, a direction or a whole
transform. Extending it has to keep every M4 spec, log and replay path
working unchanged (MF-CMD-004…006).

## Decision

**A multi-object command contract, extended additively** (Phase 1 plan
D6). Specs gain an `operands:` section: `{ name, role: read | consumed |
kept, kinds: [object, face_set, component], required }`. It is optional
(`#[serde(default)]`), so every spec YAML written before Phase 1 still
validates unchanged — the schema and `mf-spec` tests for this are
`operands_section_validates_against_schema` and the untouched
`builtin_specs_validate_against_schema`.

Downstream of the spec (Task 3 onward):
- `Operation::execute_doc` takes every operand and returns
  `ObjectChange`s (geometry, transform, object created, object removed),
  with a default implementation that wraps the existing `execute` on
  operand 0, so M4 operations don't change.
- `Sources` gains `operands: Vec<(u32, FaceId)>`, faces of other
  operands, with `#[serde(default)]`, so provenance can name a face of
  any operand, not just the target.
- `CommandRecord` gains `operands` and `outputs` (per object: hashes and
  transform), also with `#[serde(default)]`. The log format moves to
  `mfx-log/2`; `mfx-log/1` logs still replay.
- A command's verdict is the worst verdict among its objects.

**Transforms are recorded as the absolute resulting matrix** (D7). Specs
gain the parameter types `vec3` (`ParamKind::Vec3 { default: [f64; 3] }`,
`ParamValue::Vec3([f64; 3])`) and `affine` (`ParamKind::Affine { default:
[f64; 12] }`, `ParamValue::Affine([f64; 12])`), serialized as JSON arrays
in a command log and comma-separated on the CLI. `object.set_transform`
records the 12 f64 values of the resulting matrix; the gizmo and numeric
fields compute that matrix on the UI side (tiny arithmetic, not
geometry), so replay runs no trigonometry and is Exact on every OS.

`ParamSpec.unit: Some("mm")` marks a parameter (float or vec3) as a
length. `OperationSpec::to_document_units(&mut Params, unit: Unit)`
converts every such parameter from millimetres to the document's units
and runs once, in `prepare` — the Global Constraints' "Printing
parameters are in document units... converted at prepare time. The
converted value is what's recorded," and D20's "everything is converted
to document units when the command is prepared."

## Alternatives considered

- **A separate document-op path that bypasses specs.** Rejected: it
  breaks MF-SPEC-003 (GUI, CLI and MCP all derive from one spec) and
  leaves plugins and scripts (MF-PLUG-001, MF-SCRIPT-001) nothing to
  reuse.
- **Encoding the second object as an integer parameter.** Rejected: an
  integer gives no provenance (which face of the tool made which face of
  the result) and no validation (kind, required-ness) at the spec level.
- **Recording translate, rotate and scale deltas instead of the
  resulting matrix.** Rejected: platform `libm` `sin`/`cos` differ by
  platform, so a replayed delta could drift, and deltas compound across
  a long edit history. The resulting matrix needs no trigonometry to
  replay.

## Consequences

- A log format bump (`mfx-log/2`); old logs keep reading (Task 3).
- Every reader of `ParamKind`/`ParamValue` that matches on it
  exhaustively without a wildcard arm must add the two new variants.
  Checked workspace-wide with `cargo check --workspace --all-targets`
  after this task; the existing call sites in `mf-desktop` (`panels.rs`,
  `worker.rs`) already match with wildcard arms, so they didn't need to
  change.
- `mf-spec` gains a dependency on `mf-io` (for `mf_io::threemf::Unit`,
  the document-units type already used by `mf-document` and
  `mf-command`). `mf-io` does not depend on `mf-spec`, so this introduces
  no cycle.
- Scripting may later want delta helpers (move-by, rotate-by); those can
  be added as more operations that compute an absolute matrix
  internally, without changing the recorded contract.
