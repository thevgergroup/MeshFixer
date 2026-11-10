# Task 2 report: Phase 1 spec, ADRs and the operand contract in specs

**Status: done.** `docs/spec/phase1-spec.md`, `docs/adr/0003-multi-object-commands.md` and `docs/adr/0004-mixing-join.md` created. `specs/operation.schema.json` gains an optional `operands:` section and the `vec3`/`affine` parameter types. `crates/mf-spec/src/{model.rs, params.rs, cli.rs}` gain `OperandSpec`/`OperandRole`, `ParamKind::Vec3`/`Affine`, `ParamValue::Vec3`/`Affine`, and `OperationSpec::to_document_units`. ADR-0002 untouched (status still NO-GO, per controller notes). No new external dependency, so `THIRD_PARTY_NOTICES.md` is unchanged (`mf-spec` → `mf-io` is an in-workspace edge only, no cycle: `mf-io` doesn't depend on `mf-spec`).

## RED -> GREEN

New test file `crates/mf-spec/tests/operands_and_params.rs` (3 tests, all written first and confirmed failing with compile errors — `ParamValue::Vec3`, `OperandRole`, `OperationSpec.operands` and `to_document_units` didn't exist):
- `vec3_and_affine_params_validate_and_round_trip`
- `operands_section_validates_against_schema`
- `lengths_convert_to_document_units`

All three pass after implementation. The existing parity test (`parity_specs_and_impls`, actually in `crates/mf-operations/tests/ops.rs`, not `mf-spec` — the brief's "existing parity test" referred to this one) still passes unchanged, since no new operation specs were added in this task.

## Test summary

`cargo test -j 2 -p mf-spec`: 14 passed (3 new + 11 existing), 0 failed.
`cargo clippy -j 2 -p mf-spec --all-targets -- -D warnings`: clean.
`cargo check -j 2 --workspace --all-targets`: clean (confirms the non-exhaustive `ParamKind`/`ParamValue` matches in `mf-desktop` panels.rs/worker.rs, which all have wildcard arms, don't break).
`cargo test -j 2 -p mf-operations --test ops parity_specs_and_impls`: passes.

## Design notes

- `Unit` is `mf_io::threemf::Unit` (the same type `mf-document`/`mf-command` already use for document units), so `to_document_units` matches the brief's literal signature without inventing a parallel unit type. `mf-spec` now depends on `mf-io`; verified no reverse dependency exists.
- `vec3`/`affine` YAML defaults parse through a new `Scalar::Array(Vec<Scalar>)` variant and `Scalar::as_f64_array::<N>()`, which enforces exact element count by name (never truncates).
- CLI/command-log string form is comma-separated (`parse_f64_csv::<N>`), rejecting a wrong count or non-finite values by name.
- `to_document_units` scales `float` and `vec3` parameters marked `unit: mm`; `affine` is never marked `unit: mm` (D7: the resulting transform is computed by the UI side already in document units, not converted post-hoc) and non-length kinds are untouched.
- `phase1-spec.md` numbers MF-P1-CMD-001 (operands/object changes + D7's vec3/affine), MF-P1-SEL-001 (operand provenance), MF-P1-VAL-001 (D4, kernel-independent), MF-P1-SCALE-001 (D5, kernel-independent), a performance-targets table per milestone, and MF-P1-UC-001…003 with scenario assertions copied verbatim from Tasks 16a/24a/30. It notes kernel qualification is pending re-qualification (Task 7b) and never references ADR-0002's decision by number.
- ADR-0004 explicitly separates the join/closure *rules* (D14/D15) from the kernel question, so Task 2's spec work isn't blocked by ADR-0002's NO-GO.

## Concerns

- `phase1-spec.md` and the ADRs describe several Task 3+ behaviors (operand provenance totality, `ObjectChange`, the window, the validator) that don't exist in code yet — marked as forward-looking in the spec text and attributed to the task that will implement them, consistent with how `phase0-spec.md` §11a/§11b documented not-yet-built requirements.
- `OperandRole` and the schema's `operands[].role` enum (`read|consumed|kept`) match the brief exactly, but no operation in this task actually declares `operands:` yet (that starts at Task 3's `mesh.boolean` etc.), so the only coverage is the schema/model round-trip test, not an end-to-end operand-bearing operation.
