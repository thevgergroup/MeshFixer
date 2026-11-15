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
    DocContext, DocOutput, ObjectChange, OpError, OpOutput, OperandInput, Operation, RunContext,
    Target, needs_document, unsupported_target,
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
        "blend_needs_union" => (
            "Only a union can be blended",
            "Set the blend width to 0, or join with a union",
        ),
        "blend_self_intersects" => ("The blend folds over itself", "Try a narrower blend"),
        "blend_did_not_converge" => (
            "The blend couldn't be smoothed",
            "Try a different blend width",
        ),
        "blend_too_wide" => ("The blend is wider than the model", "Use a narrower blend"),
        "blend_too_narrow" => (
            "The blend is narrower than the faces at the seam",
            "Use a wider blend",
        ),
        "stamp_not_embedded" => (
            "The part's open side isn't inside the model",
            "Move the part so its open side sinks into the surface, or raise the embed depth",
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
        let blend_width = params.get_float("blend_width")?;
        if blend_width > 0.0 && op != BoolOp::Union {
            return Err(refuse(
                "blend_needs_union",
                format!(
                    "blend_width {blend_width} with {}",
                    params.get_choice("operation")?
                ),
            ));
        }
        let in_frame =
            operand_in_frame(tool.mesh, &tool.transform, &target.transform).ok_or_else(|| {
                OpError::Failed {
                    code: "singular_transform",
                    message: "the target's transform can't be inverted".into(),
                }
            })?;
        // An open part (a stamp, D14) is capped flat and sunk into the
        // target along its cap's outward normal before a union.
        let tool_faces = in_frame.face_slots();
        let (in_frame, embedded) =
            if op == BoolOp::Union && !in_frame.topology_with(ctx)?.boundary_loops.is_empty() {
                let depth = match params.get_float("embed_depth")? {
                    d if d > 0.0 => d,
                    _ => 0.5 * blend_width,
                };
                (cap_and_sink(&in_frame, depth, doc, ctx)?, true)
            } else {
                (in_frame, false)
            };
        let out = boolean(target.mesh, &in_frame, op, kernel(), ctx).map_err(|e| match e {
            BooleanError::Cancelled => OpError::Cancelled,
            e => refuse(e.code(), e.to_string()),
        })?;
        if embedded {
            // The cap is temporary: none of the result may come from it.
            let from_cap = out
                .provenance
                .created_faces
                .iter()
                .any(|(_, s)| s.operands.iter().any(|&(k, f)| k == 1 && f.0 >= tool_faces));
            if from_cap {
                return Err(refuse(
                    "stamp_not_embedded",
                    "the cap over the part's open side would be left in the result".into(),
                ));
            }
        }
        let mut out = out;
        if blend_width > 0.0 {
            let (mesh, provenance) =
                crate::blend::blend(out.mesh, out.provenance, 1, blend_width, ctx)?;
            out.mesh = mesh;
            out.provenance = provenance;
        }
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
                "blend_width": blend_width,
                "stamp_capped": embedded,
            }),
        })
    }
}

/// An open part capped flat (`mesh.fill_holes`, flat, unrefined) and moved
/// `depth` along its caps' outward normal (into the surface it's joined
/// to). The cap faces come after the part's own, so they can be told apart
/// in the union's provenance.
fn cap_and_sink(
    part: &Mesh,
    depth: f64,
    doc: &DocContext,
    ctx: &RunContext,
) -> Result<Mesh, OpError> {
    let fill = crate::find("mesh.fill_holes").expect("mesh.fill_holes is registered");
    let spec = mf_spec::builtin::get("mesh.fill_holes").expect("its spec is built in");
    let raw = [("fill_style", "flat"), ("refine", "false")]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let params = spec.validate(&raw)?;
    let capped = match fill
        .execute(part, &Target::WholeObject, &params, doc, ctx)?
        .change
    {
        Some((mesh, _)) => mesh,
        None => part.clone(),
    };
    let mut normal = [0.0; 3];
    for f in part.face_slots()..capped.face_slots() {
        if let Some(t) = capped.face(mf_geometry::FaceId(f)) {
            let [a, b, c] = t.map(|v| capped.corner_position(v));
            let (u, w) = (
                [b[0] - a[0], b[1] - a[1], b[2] - a[2]],
                [c[0] - a[0], c[1] - a[1], c[2] - a[2]],
            );
            normal[0] += u[1] * w[2] - u[2] * w[1];
            normal[1] += u[2] * w[0] - u[0] * w[2];
            normal[2] += u[0] * w[1] - u[1] * w[0];
        }
    }
    let len = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
    if len.is_nan() || len <= 0.0 || depth == 0.0 {
        return Ok(capped);
    }
    let shift = normal.map(|c| c / len * depth);
    let mut edit = capped.edit();
    for (v, p) in capped.vertices() {
        edit.move_vertex(v, [p[0] + shift[0], p[1] + shift[1], p[2] + shift[2]])
            .expect("a live vertex moves to a finite position");
    }
    Ok(edit
        .finish()
        .expect("a moved copy of a valid mesh is valid")
        .0)
}
