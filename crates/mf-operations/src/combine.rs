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

use std::borrow::Cow;
use std::collections::BTreeMap;

use mf_boolean::{BoolOp, BooleanError, boolean, operand_in_frame};
use mf_geometry::{Mesh, Provenance};
use mf_parts::ShapeRole;
use mf_spec::Params;

use crate::boolean::kernel;
use crate::refusal::RefusalArg;
use crate::{
    DocContext, DocOutput, ObjectChange, OpError, OpOutput, OperandInput, Operation, RunContext,
    Target, Unit, needs_document, unsupported_target,
};

pub struct Combine;

/// Refusal codes about the accumulated target rather than the object
/// being joined or cut in that step.
const TARGET_CODES: &[&str] = &[
    "operand_not_closed",
    "operand_overlaps_itself",
    "section_self_intersects",
    "window_degenerate",
];

/// A refusal naming `object` first, then the copy table's "what"/"next"
/// words composed with `detail` exactly as the pre-registry text did
/// (ADR-0010: built through the one registry helper, with `object` and
/// the composed `detail` as the named arguments
/// `specs/operations/mesh.boolean.yaml`'s message fills in — the same
/// codes and copy `mesh.boolean` itself refuses with).
fn refuse_naming(object: &str, code: &'static str, detail: String) -> OpError {
    let message = match crate::boolean::refusal_copy(code) {
        Some((what, next)) => format!("{what} ({detail}). {next}."),
        None => detail,
    };
    crate::refusal::refuse(
        code,
        BTreeMap::from([
            ("object", RefusalArg::Text(object.to_owned())),
            ("detail", RefusalArg::Text(message)),
        ]),
    )
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
        _: &DocContext,
        _: &RunContext,
    ) -> Result<OpOutput, OpError> {
        Err(needs_document(self.id()))
    }

    fn execute_doc(
        &self,
        inputs: &[OperandInput],
        params: &Params,
        _doc: &DocContext,
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
            return Err(crate::refusal::refuse(
                "nothing_to_combine",
                BTreeMap::from([("object", RefusalArg::Text(target.name.to_owned()))]),
            ));
        }
        let keep_holes = params.get_bool("keep_holes")?;
        if let Some(other) = rest
            .iter()
            .find(|o| o.slot != "solids" && o.slot != "holes")
        {
            return Err(OpError::Failed {
                code: "missing_operand",
                message: format!("{} has no operand slot '{}'", self.id(), other.slot),
            });
        }
        let parts: Vec<PartIn> = rest
            .iter()
            .enumerate()
            .map(|(i, o)| PartIn {
                name: o.name.to_owned(),
                mesh: std::borrow::Cow::Borrowed(o.mesh),
                transform: o.transform,
                role: if o.slot == "holes" {
                    ShapeRole::Hole
                } else {
                    ShapeRole::Solid
                },
                host: 0,
                operand: Some(i + 1),
            })
            .collect();
        let mut applied = apply_parts(
            [Some((0, target)), None],
            &parts,
            ApplyOrder::Sequential,
            ctx,
        )?;
        let Applied {
            mesh,
            provenance,
            steps: report_steps,
            ..
        } = applied.remove(0);
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

// ---------------------------------------------------------------------
// The one apply path: N parts folded into their targets.
// ---------------------------------------------------------------------

/// One part [`apply_parts`] folds into a target: a solid joined or a hole
/// cut, from wherever it's placed.
pub struct PartIn<'a> {
    /// Its name, for a refusal ("Dowel 1 (hole)").
    pub name: String,
    pub mesh: Cow<'a, Mesh>,
    /// Where it's placed (it's brought into its target's frame, D10).
    pub transform: [f64; 12],
    pub role: ShapeRole,
    /// Which target: 0 or 1 (`hosts`' index).
    pub host: usize,
    /// The command operand it is, if it's an object (its faces' sources
    /// then name it); `None` for a generated part.
    pub operand: Option<usize>,
}

/// How a target's parts are applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOrder {
    /// One boolean per part: the solids in order, then the holes
    /// (`mesh.combine`, `object.attach`).
    Sequential,
    /// Every hole as one tool, then every solid as one tool
    /// (`split.place_connectors`, recorded that way).
    MergedHolesFirst,
}

/// A target with its parts applied.
pub struct Applied {
    /// `hosts`' index.
    pub host: usize,
    pub mesh: Mesh,
    /// Relative to the target's input; a part's faces name its operand.
    pub provenance: Provenance,
    /// One report row per boolean step.
    pub steps: Vec<serde_json::Value>,
}

/// Folds `parts` into their targets (`hosts`: each the command operand
/// index and its input, or `None` when the command has no such target):
/// every solid unioned, every hole cut, each step checked by the boolean
/// kernel and validator. One result per target that got any part. The
/// one place a generated or placed part becomes geometry: `mesh.combine`,
/// `object.attach`, `connector.join` and `split.place_connectors` all
/// come here.
pub fn apply_parts(
    hosts: [Option<(usize, &OperandInput)>; 2],
    parts: &[PartIn],
    order: ApplyOrder,
    ctx: &RunContext,
) -> Result<Vec<Applied>, OpError> {
    let mut out = Vec::new();
    for (h, host) in hosts.iter().enumerate() {
        let mine: Vec<&PartIn> = parts.iter().filter(|p| p.host == h).collect();
        if mine.is_empty() {
            continue;
        }
        let Some((_, target)) = host else {
            return Err(OpError::Failed {
                code: "missing_operand",
                message: format!(
                    "'{}' joins a second object, but none was given.",
                    mine[0].name
                ),
            });
        };
        // Each step: the tool (in the target's frame), its op, its name
        // and the operand its faces come from.
        let into_frame = |p: &PartIn| {
            operand_in_frame(&p.mesh, &p.transform, &target.transform).ok_or_else(|| {
                OpError::Failed {
                    code: "singular_transform",
                    message: format!(
                        "'{}': the transform can't be inverted; reset it first",
                        target.name
                    ),
                }
            })
        };
        let mut steps: Vec<(Mesh, BoolOp, String, Option<usize>)> = Vec::new();
        match order {
            ApplyOrder::Sequential => {
                for (role, op) in [
                    (ShapeRole::Solid, BoolOp::Union),
                    (ShapeRole::Hole, BoolOp::Subtract),
                ] {
                    for p in mine.iter().filter(|p| p.role == role) {
                        steps.push((into_frame(p)?, op, p.name.clone(), p.operand));
                    }
                }
            }
            ApplyOrder::MergedHolesFirst => {
                for (role, op) in [
                    (ShapeRole::Hole, BoolOp::Subtract),
                    (ShapeRole::Solid, BoolOp::Union),
                ] {
                    let list: Vec<Mesh> = mine
                        .iter()
                        .filter(|p| p.role == role)
                        .map(|p| into_frame(p))
                        .collect::<Result<_, _>>()?;
                    if let Some(first) = mine.iter().find(|p| p.role == role) {
                        let name = if list.len() == 1 {
                            first.name.clone()
                        } else {
                            format!("{} and {} more", first.name, list.len() - 1)
                        };
                        steps.push((crate::split::pegs::merged(&list), op, name, None));
                    }
                }
            }
        }
        let mut mesh = target.mesh.clone();
        let mut provenance: Option<Provenance> = None;
        let mut report = Vec::with_capacity(steps.len());
        for (tool, op, name, operand) in steps {
            ctx.checkpoint()?;
            let r = boolean(&mesh, &tool, op, kernel(), ctx).map_err(|e| match e {
                BooleanError::Cancelled => OpError::Cancelled,
                e => {
                    let code = e.code();
                    let at = if TARGET_CODES.contains(&code) {
                        target.name
                    } else {
                        name.as_str()
                    };
                    refuse_naming(at, code, e.to_string())
                }
            })?;
            let mut step = r.provenance;
            for (_, s) in &mut step.created_faces {
                match operand {
                    Some(k) => {
                        for (o, _) in &mut s.operands {
                            if *o == 1 {
                                *o = k as u32;
                            }
                        }
                    }
                    None => s.operands.retain(|&(o, _)| o != 1),
                }
            }
            provenance = Some(match provenance {
                None => step,
                Some(acc) => crate::blend::compose(&acc, &step),
            });
            report.push(serde_json::json!({
                "object": name,
                "operation": if op == BoolOp::Union { "union" } else { "subtract" },
                "window_faces": r.window_faces,
            }));
            mesh = r.mesh;
        }
        out.push(Applied {
            host: h,
            mesh,
            provenance: provenance.expect("at least one step ran"),
            steps: report,
        });
    }
    Ok(out)
}

/// The gap two attached targets keep where they meet (a hinge's moving
/// parts): refused as `moving_parts_fused` if it's under `gap` (less a
/// tenth for the chord tolerance). Returns the measured distance (`None`
/// when they're further apart than the measure looks). `unit` is the
/// document's own unit (consolidation plan C14): `gap` and the measured
/// distance are document-unit `f64`s (D20), carried into the refusal as
/// [`mf_units::Length`] rather than a pre-formatted string with a
/// hard-coded "mm" and `{:.3}` (the owner's bug this fixes: a "mm"
/// gap message on a metres document).
pub fn check_moving_gap(
    a: (&Mesh, &OperandInput),
    b: (&Mesh, &OperandInput),
    gap: f64,
    unit: Unit,
    ctx: &RunContext,
) -> Result<Option<f64>, OpError> {
    let bvh = mf_geometry::Bvh::build(a.0, ctx)?;
    let b_in_a =
        operand_in_frame(b.0, &b.1.transform, &a.1.transform).ok_or_else(|| OpError::Failed {
            code: "singular_transform",
            message: format!("'{}': the transform can't be inverted", b.1.name),
        })?;
    let measured = mf_geometry::minimum_distance(a.0, &bvh, &b_in_a, &glam::DMat4::IDENTITY, ctx)?;
    let tolerance = (gap * 0.1).max(1e-6);
    if let Some(d) = measured
        && d < gap - tolerance
    {
        return Err(crate::refusal::refuse(
            "moving_parts_fused",
            BTreeMap::from([
                ("a", RefusalArg::Text(a.1.name.to_owned())),
                ("b", RefusalArg::Text(b.1.name.to_owned())),
                (
                    "measured",
                    RefusalArg::Length(mf_units::Length::new(d, unit)),
                ),
                (
                    "expected",
                    RefusalArg::Length(mf_units::Length::new(gap, unit)),
                ),
            ]),
        ));
    }
    Ok(measured)
}
