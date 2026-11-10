//! `mesh.boolean` (MF-MODEL-001): union, subtract or intersect the target
//! with a tool object, on the target's local window (D5), with the C++
//! Manifold kernel and its operand check (ADR-0002 option B), validated
//! independently (D4). The tool is brought into the target's frame (D10);
//! the target keeps its id, name and transform, and the tool is removed
//! unless `keep_tool` is set.
//!
//! Every refusal is an [`OpError::Failed`] whose code is in the copy table
//! ([`refusal_copy`]): a plain-language reason and the next action.

use mf_boolean::{BoolOp, BooleanError, Kernel, ManifoldCpp, boolean, operand_in_frame};
use mf_geometry::Mesh;
use mf_spec::Params;

use crate::{
    DocOutput, ObjectChange, OpError, OpOutput, OperandInput, Operation, RunContext, Target,
    needs_document, unsupported_target,
};

pub struct Boolean;

/// The kernel `mesh.boolean` runs (ADR-0002 option B).
pub fn kernel() -> &'static dyn Kernel {
    static K: ManifoldCpp = ManifoldCpp::checked();
    &K
}

/// The copy table for `mesh.boolean`'s refusals: what went wrong in plain
/// language, and the next action. `None` for a code that isn't a boolean
/// refusal.
pub fn refusal_copy(code: &str) -> Option<(&'static str, &'static str)> {
    Some(match code {
        "operand_not_closed" => (
            "The model has holes; cutting needs a closed model",
            "Fill all holes, then combine",
        ),
        "tool_not_closed" => (
            "The tool isn't a closed solid",
            "Fill the tool's holes first",
        ),
        "opening_not_covered" => (
            "The part doesn't cover the opening it was joined over",
            "Move the part to cover the opening, or fill the hole first",
        ),
        "opening_too_large_for_join" => (
            "An opening near the part is too large to close for the join",
            "Fill the hole first",
        ),
        "no_overlap" => ("These objects don't touch", "Move one onto the other"),
        "operand_overlaps_itself" => (
            "This part of the model overlaps itself near the tool",
            "Run Make solid on it first, or move the tool",
        ),
        "section_self_intersects" => (
            "The model folds onto itself where the tool's working area cuts it",
            "Run Make solid on it first, or move the tool slightly",
        ),
        "result_not_manifold" => (
            "The objects only touch along an edge or at a point, so the result wouldn't be a clean solid",
            "Move one slightly so they overlap",
        ),
        "result_self_intersects" => (
            "The result would intersect itself",
            "Move the tool slightly and try again",
        ),
        "result_empty" => (
            "Nothing would be left",
            "Check the operation: subtracting the whole object leaves nothing",
        ),
        "kernel_internal" => (
            "The boolean engine failed on this input",
            "Move the tool slightly and try again, or report it",
        ),
        "blend_not_available" => (
            "Blended seams aren't available yet",
            "Set the blend width to 0",
        ),
        _ => return None,
    })
}

fn refuse(code: &'static str, detail: String) -> OpError {
    let message = match refusal_copy(code) {
        Some((what, next)) => format!("{what} ({detail}). {next}."),
        None => detail,
    };
    OpError::Failed { code, message }
}

impl Operation for Boolean {
    fn id(&self) -> &'static str {
        "mesh.boolean"
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
        let [target, tool] = inputs else {
            return Err(OpError::Failed {
                code: "missing_operand",
                message: format!("{} needs a target and a tool", self.id()),
            });
        };
        for o in [target, tool] {
            if o.target != Target::WholeObject {
                return Err(unsupported_target(self.id(), &o.target));
            }
        }
        let op = match params.get_choice("operation")? {
            "union" => BoolOp::Union,
            "subtract" => BoolOp::Subtract,
            "intersect" => BoolOp::Intersect,
            other => {
                return Err(OpError::Failed {
                    code: "invalid_parameter",
                    message: format!("unknown operation '{other}'"),
                });
            }
        };
        let keep_tool = params.get_bool("keep_tool")?;
        if params.get_float("blend_width")? != 0.0 {
            return Err(refuse(
                "blend_not_available",
                "blend_width must be 0 in this version".into(),
            ));
        }
        let in_frame =
            operand_in_frame(tool.mesh, &tool.transform, &target.transform).ok_or_else(|| {
                OpError::Failed {
                    code: "singular_transform",
                    message: "the target's transform can't be inverted".into(),
                }
            })?;
        let out = boolean(target.mesh, &in_frame, op, kernel(), ctx).map_err(|e| match e {
            BooleanError::Cancelled => OpError::Cancelled,
            e => refuse(e.code(), e.to_string()),
        })?;
        let created = out.provenance.created_faces.len();
        let removed = out.provenance.removed_faces.len();
        let kept = out.mesh.face_count() - created;
        let mut changes = vec![ObjectChange::Geometry {
            operand: 0,
            mesh: out.mesh,
            provenance: out.provenance,
        }];
        if !keep_tool {
            changes.push(ObjectChange::Remove { operand: 1 });
        }
        Ok(DocOutput {
            changes,
            report: serde_json::json!({
                "operation": params.get_choice("operation")?,
                "kernel": kernel().id(),
                "created_faces": created,
                "removed_faces": removed,
                "unchanged_faces": kept,
                "window_faces": out.window_faces,
                "tool_kept": keep_tool,
            }),
        })
    }
}
