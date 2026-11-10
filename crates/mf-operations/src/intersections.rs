//! `mesh.intersections` (MF-MODEL-002, Phase 1 Task 9): read-only. Where
//! the target crosses another object (operand `other`), or itself. The
//! other object is placed in the target's frame exactly as `mesh.boolean`
//! places its tool (D10), so detection and the boolean agree.

use mf_geometry::affine::{to_dmat4, tool_to_target};
use mf_geometry::{Bvh, IntersectionReport, Mesh, intersections_between, self_intersections};
use mf_spec::Params;
use serde_json::{Value, json};

use crate::{
    DocOutput, OpError, OpOutput, OperandInput, Operation, RunContext, Target, unsupported_target,
};

pub struct Intersections;

fn report_json(r: &IntersectionReport, with: Option<&str>) -> Value {
    json!({
        "with": with,
        "pair_count": r.pair_count,
        "regions": r.regions.iter().map(|g| json!({
            "faces_a": g.faces_a.len(),
            "faces_b": g.faces_b.len(),
            "extent": g.extent,
            "centroid": g.centroid,
            "segments": g.segments.len(),
        })).collect::<Vec<_>>(),
    })
}

impl Operation for Intersections {
    fn id(&self) -> &'static str {
        "mesh.intersections"
    }

    fn version(&self) -> u32 {
        1
    }

    fn execute(
        &self,
        input: &Mesh,
        target: &Target,
        _: &Params,
        ctx: &RunContext,
    ) -> Result<OpOutput, OpError> {
        if *target != Target::WholeObject {
            return Err(unsupported_target(self.id(), target));
        }
        ctx.progress.report("finding intersections", 0.0);
        let bvh = Bvh::build(input, ctx)?;
        let r = self_intersections(input, &bvh, ctx)?;
        ctx.progress.report("finding intersections", 1.0);
        Ok(OpOutput {
            change: None,
            report: report_json(&r, None),
        })
    }

    fn execute_doc(
        &self,
        inputs: &[OperandInput],
        params: &Params,
        ctx: &RunContext,
    ) -> Result<DocOutput, OpError> {
        let report = match inputs {
            [target] => {
                self.execute(target.mesh, &target.target, params, ctx)?
                    .report
            }
            [target, other] => {
                for o in [target, other] {
                    if o.target != Target::WholeObject {
                        return Err(unsupported_target(self.id(), &o.target));
                    }
                }
                let t = tool_to_target(&other.transform, &target.transform).ok_or_else(|| {
                    OpError::Failed {
                        code: "singular_transform",
                        message: "the target's transform can't be inverted".into(),
                    }
                })?;
                ctx.progress.report("finding intersections", 0.0);
                let a_bvh = Bvh::build(target.mesh, ctx)?;
                let b_bvh = Bvh::build(other.mesh, ctx)?;
                let r = intersections_between(
                    target.mesh,
                    &a_bvh,
                    other.mesh,
                    &b_bvh,
                    &to_dmat4(&t),
                    ctx,
                )?;
                ctx.progress.report("finding intersections", 1.0);
                report_json(&r, Some(&other.object.to_string()))
            }
            _ => {
                return Err(OpError::Failed {
                    code: "missing_target",
                    message: format!("{} takes a target and at most one other object", self.id()),
                });
            }
        };
        Ok(DocOutput {
            changes: Vec::new(),
            report,
        })
    }
}
