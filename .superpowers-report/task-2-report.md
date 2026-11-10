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

## Fix round 1

Addressed review findings:

1. **Critical (conversion never called).** `Session::prepare` (`crates/mf-command/src/session.rs`) now calls `spec.to_document_units(&mut params, self.document().units())` right after `spec.validate`, before `prepare_validated`. Converted values are what gets recorded.
2. **Idempotency.** Conversion must happen exactly once, at the original `prepare`, never again when a recorded command is carried forward. Added `Session::execute_recorded(op_id, target, selection, params)`, a non-converting path (`prepare_validated` → `run` → `commit`) that both `reexecute` (refactored to call it) and `replay::run_and_classify` (changed from re-stringifying `record.params` into an `Invocation` and calling `execute`, which would have re-converted) now use. Also exposed `Prepared::params()` for test inspection.
   - New test file `crates/mf-command/tests/unit_conversion.rs`, driven against a new test-only operation `test.unit_conversion` (one `unit: mm` float param, `clearance`), gated behind a new `test-kinds` Cargo feature on `mf-spec` and `mf-operations` (mirroring `mf-document`'s existing `test-kinds` convention), enabled only as `mf-command`'s dev-dependency. New files: `specs/test-operations/test.unit_conversion.yaml`, `crates/mf-operations/src/test_unit_conversion.rs`.
   - 6 tests, all written first and confirmed failing (by temporarily disabling the `to_document_units` call and observing 4 of 6 fail with the unconverted 0.2 instead of 0.0002) before re-enabling the fix:
     - `clearance_in_mm_is_recorded_in_document_units` (a): 0.2 mm in a metre document records as 0.0002, and the operation itself only ever observes the converted value.
     - `clearance_in_mm_is_unchanged_in_a_millimetre_document`: sanity check, ×1.0.
     - `preparing_the_same_invocation_twice_gives_identical_params` (c): two `prepare()` calls on the same invocation give bit-identical `Params`.
     - `replay_uses_the_recorded_value_unchanged` (b): save a log, replay it, the replayed command's recorded clearance is exactly the saved 0.0002.
     - `recorded_params_carried_onto_a_different_unit_document_are_unchanged` (b, extended): recorded params from a metre document, carried via `execute_recorded` onto a *different* (inch) document, stay 0.0002 — not reconverted for the new document's unit. (`replay()` itself always reconstructs the document the log describes, so it can't exercise a genuinely different current-document unit; this test exercises the same non-converting path through `execute_recorded` directly.)
     - `reexecute_uses_the_recorded_value_unchanged`: `reexecute` (MF-HIST-002) also carries the recorded value forward unchanged.
3. **Minor.** Added a `$comment` to `specs/operation.schema.json`'s `parameters[].default` noting vec3/affine arity (3 / 12 numbers) is enforced in Rust deserialization (`mf-spec::model::Scalar::as_f64_array`, `TryFrom<RawParam> for ParamSpec`), not by the schema itself.

Gate: `cargo fmt --all` (clean, no diff), `cargo clippy -j 2 -p mf-spec -p mf-command --all-targets -- -D warnings` (clean), `cargo test -j 2 -p mf-spec -p mf-command` (all green: `mf-spec` 14/14, `mf-command` 65/65 including the 6 new tests), `cargo check -j 2 --workspace --all-targets` (clean). Also re-ran `mf-operations`' full suite (with and without `--features test-kinds`) and its `parity_specs_and_impls` test specifically: all green, including with the feature on (the test-only spec and its matching registered operation stay in parity).
