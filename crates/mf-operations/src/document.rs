//! `document.reinterpret_units` and `document.scale_units` (consolidation
//! plan C07, units-audit.md §3, import-units-brief.md "Fix it
//! afterwards"): the Inspect "Units" control's two actions, both recorded,
//! undoable commands that replay Exact.
//!
//! - **Reinterpret:** "the numbers were in X, not Y" — the document's own
//!   unit changes, but no object's transform or geometry does. The same
//!   coordinates now mean a different physical size.
//! - **Scale:** the physical size changes; the document's unit is
//!   unchanged. Every target's transform is scaled by the ratio between
//!   the given unit and the document's unit, exactly as a cross-unit
//!   import scales a newly imported mesh (`Session::import_model`).

use mf_geometry::Mesh;
use mf_spec::Params;
use mf_units::Length;

use crate::{
    DocContext, DocOutput, ObjectChange, OpError, OpOutput, OperandInput, Operation, RunContext,
    Target, needs_document, unsupported_target,
};

fn failed(code: &'static str, message: impl Into<String>) -> OpError {
    OpError::Failed {
        code,
        message: message.into(),
    }
}

/// Parses the `unit` param (a [`mf_spec::ParamKind::Choice`] of symbols:
/// `um`, `mm`, `cm`, `m`, `in`, `ft`) into a [`Unit`](mf_units::Unit).
/// The spec's `values:` list is exactly [`Unit::parse_symbol`]'s accepted
/// set, so this never fails for a value the spec itself validated;
/// `invalid_unit` exists only so a future spec/vocabulary mismatch is a
/// refusal by name, never a panic.
fn unit_param(params: &Params, op: &str) -> Result<mf_units::Unit, OpError> {
    let word = params.get_choice("unit")?;
    mf_units::Unit::parse_symbol(word)
        .ok_or_else(|| failed("invalid_unit", format!("{op}: '{word}' isn't a known unit")))
}

/// `document.reinterpret_units`: relabels the document's unit. Every
/// object's transform and geometry stay bit-for-bit what they were.
pub struct ReinterpretUnits;

impl Operation for ReinterpretUnits {
    fn id(&self) -> &'static str {
        "document.reinterpret_units"
    }

    fn version(&self) -> u32 {
        1
    }

    fn execute(
        &self,
        _: &Mesh,
        _: &Target,
        _: &Params,
        _: &DocContext,
        _: &RunContext,
    ) -> Result<OpOutput, OpError> {
        Err(needs_document(self.id()))
    }

    fn execute_doc(
        &self,
        inputs: &[OperandInput],
        params: &Params,
        doc: &DocContext,
        _ctx: &RunContext,
    ) -> Result<DocOutput, OpError> {
        let target = inputs.first().ok_or_else(|| {
            failed(
                "missing_target",
                format!("{} was run with no target", self.id()),
            )
        })?;
        if target.target != Target::WholeObject {
            return Err(unsupported_target(self.id(), &target.target));
        }
        let unit = unit_param(params, self.id())?;
        let changes = if unit == doc.unit {
            Vec::new()
        } else {
            vec![ObjectChange::Units { unit }]
        };
        Ok(DocOutput {
            changes,
            report: serde_json::json!({
                "from": doc.unit.symbol(),
                "to": unit.symbol(),
            }),
        })
    }
}

/// `document.scale_units`: scales every target's transform (never its
/// geometry) so the model's physical size becomes what it would be if
/// its coordinates had actually been in `unit`, read in the document's
/// own unit. The document's unit is unchanged.
pub struct ScaleUnits;

impl Operation for ScaleUnits {
    fn id(&self) -> &'static str {
        "document.scale_units"
    }

    fn version(&self) -> u32 {
        1
    }

    fn execute(
        &self,
        _: &Mesh,
        _: &Target,
        _: &Params,
        _: &DocContext,
        _: &RunContext,
    ) -> Result<OpOutput, OpError> {
        Err(needs_document(self.id()))
    }

    fn execute_doc(
        &self,
        inputs: &[OperandInput],
        params: &Params,
        doc: &DocContext,
        _ctx: &RunContext,
    ) -> Result<DocOutput, OpError> {
        if inputs.is_empty() {
            return Err(failed(
                "missing_target",
                format!("{} was run with no target", self.id()),
            ));
        }
        for i in inputs {
            if i.target != Target::WholeObject {
                return Err(unsupported_target(self.id(), &i.target));
            }
        }
        let unit = unit_param(params, self.id())?;
        // A length of 1 `unit`, converted to the document's own unit, is
        // exactly the ratio every target's transform scales by: a part
        // whose numbers were meant as `unit` now reads as its correct
        // physical size in the document's own unit.
        let k = Length::new(1.0, unit).in_doc(doc.unit);
        let changes = if k == 1.0 {
            Vec::new()
        } else {
            inputs
                .iter()
                .enumerate()
                .map(|(operand, i)| ObjectChange::Transform {
                    operand,
                    transform: i.transform.map(|v| v * k),
                })
                .collect()
        };
        Ok(DocOutput {
            changes,
            report: serde_json::json!({
                "unit": unit.symbol(),
                "scale": k,
            }),
        })
    }
}
