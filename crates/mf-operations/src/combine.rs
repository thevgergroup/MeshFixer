//! `mesh.combine` (Tinkercad-style grouping): union every solid onto the
//! target (the first solid), then subtract every hole, as a sequence of
//! `mesh.boolean` steps on the same kernel and validator, inside one
//! operation, so one command makes one undo step and replays exactly.
//!
//! - The target keeps its id, name and transform; every other operand is
//!   brought into its frame (D10).
//! - Each step's provenance is composed onto the previous ones, and a
//!   step's tool faces (the boolean's operand 1) are renamed to the
//!   command's operand index, so the result's provenance is relative to
//!   operand 0's input and every created face names a face of the solid
//!   or hole it came from.
//! - The other solids are removed; the holes too, unless `keep_holes`.
//! - A refused step names the object at fault, with the boolean's copy
//!   table words ([`crate::boolean::refusal_copy`]).

use mf_boolean::{BoolOp, BooleanError, boolean, operand_in_frame};
use mf_geometry::{Mesh, Provenance};
use mf_spec::Params;

use crate::boolean::{kernel, refusal_copy};
use crate::{
    DocOutput, ObjectChange, OpError, OpOutput, OperandInput, Operation, RunContext, Target,
    needs_document, unsupported_target,
};

pub struct Combine;

/// Refusal codes about the accumulated target rather than the object
/// being joined or cut in that step.
const TARGET_CODES: &[&str] = &[
    "operand_not_closed",
    "operand_overlaps_itself",
    "section_self_intersects",
];

/// A refusal naming `object` first, then the copy table's words.
fn refuse_naming(object: &str, code: &'static str, detail: String) -> OpError {
    let message = match refusal_copy(code) {
        Some((what, next)) => format!("'{object}': {what} ({detail}). {next}."),
        None => format!("'{object}': {detail}"),
    };
    OpError::Failed { code, message }
}

impl Operation for Combine {
    fn id(&self) -> &'static str {
        "mesh.combine"
    }

    fn version(&self) -> u32 {
        1
    }

    fn execute(
        &self,
        _: &Mesh,
        _: &Target,
        _: &Params,
        _: &RunContext,
    ) -> Result<OpOutput, OpError> {
        Err(needs_document(self.id()))
    }

    fn execute_doc(
        &self,
        inputs: &[OperandInput],
        params: &Params,
        ctx: &RunContext,
    ) -> Result<DocOutput, OpError> {
        let Some((target, rest)) = inputs.split_first() else {
            return Err(OpError::Failed {
                code: "missing_operand",
                message: format!("{} needs at least one solid", self.id()),
            });
        };
        for o in inputs {
            if o.target != Target::WholeObject {
                return Err(unsupported_target(self.id(), &o.target));
            }
        }
        if rest.is_empty() {
            return Err(OpError::Failed {
                code: "nothing_to_combine",
                message: format!(
                    "'{}' is the only object: there is nothing to join or cut. Select more \
                     objects.",
                    target.name
                ),
            });
        }
        let keep_holes = params.get_bool("keep_holes")?;
        // Solids first, then holes, whatever order the slots came in.
        let mut steps: Vec<(usize, BoolOp)> = Vec::with_capacity(rest.len());
        for (slot, op) in [("solids", BoolOp::Union), ("holes", BoolOp::Subtract)] {
            steps.extend(
                rest.iter()
                    .enumerate()
                    .filter(|(_, o)| o.slot == slot)
                    .map(|(i, _)| (i + 1, op)),
            );
        }
        if steps.len() != rest.len() {
            let other = rest
                .iter()
                .find(|o| o.slot != "solids" && o.slot != "holes")
                .map_or("", |o| o.slot);
            return Err(OpError::Failed {
                code: "missing_operand",
                message: format!("{} has no operand slot '{other}'", self.id()),
            });
        }
        let mut mesh = target.mesh.clone();
        let mut provenance: Option<Provenance> = None;
        let mut report_steps = Vec::with_capacity(steps.len());
        for &(k, op) in &steps {
            ctx.checkpoint()?;
            let o = &inputs[k];
            let in_frame =
                operand_in_frame(o.mesh, &o.transform, &target.transform).ok_or_else(|| {
                    OpError::Failed {
                        code: "singular_transform",
                        message: format!(
                            "'{}': the transform can't be inverted; reset it first",
                            target.name
                        ),
                    }
                })?;
            let out = boolean(&mesh, &in_frame, op, kernel(), ctx).map_err(|e| match e {
                BooleanError::Cancelled => OpError::Cancelled,
                e => {
                    let code = e.code();
                    let at = if TARGET_CODES.contains(&code) {
                        target.name
                    } else {
                        o.name
                    };
                    refuse_naming(at, code, e.to_string())
                }
            })?;
            let mut step = out.provenance;
            for (_, s) in &mut step.created_faces {
                for (operand, _) in &mut s.operands {
                    if *operand == 1 {
                        *operand = k as u32;
                    }
                }
            }
            provenance = Some(match provenance {
                None => step,
                Some(acc) => crate::blend::compose(&acc, &step),
            });
            report_steps.push(serde_json::json!({
                "object": o.name,
                "operation": if op == BoolOp::Union { "union" } else { "subtract" },
                "window_faces": out.window_faces,
            }));
            mesh = out.mesh;
        }
        let provenance = provenance.expect("at least one step ran");
        let created = provenance.created_faces.len();
        let removed = provenance.removed_faces.len();
        let mut changes = vec![ObjectChange::Geometry {
            operand: 0,
            mesh,
            provenance,
        }];
        for (k, o) in rest.iter().enumerate() {
            if o.slot == "solids" || !keep_holes {
                changes.push(ObjectChange::Remove { operand: k + 1 });
            }
        }
        let solids = 1 + rest.iter().filter(|o| o.slot == "solids").count();
        Ok(DocOutput {
            changes,
            report: serde_json::json!({
                "kernel": kernel().id(),
                "solids": solids,
                "holes": rest.len() + 1 - solids,
                "holes_kept": keep_holes,
                "created_faces": created,
                "removed_faces": removed,
                "steps": report_steps,
            }),
        })
    }
}
