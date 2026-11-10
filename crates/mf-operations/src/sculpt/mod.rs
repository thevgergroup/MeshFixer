//! Sculpting (MF-SCULPT-001…004, Phase 1 Tasks 17–18): brushes, falloff,
//! symmetry and masks ([`StrokeState`]), and `mesh.sculpt_stroke`, the
//! command one stroke (press to release) becomes.
//!
//! A stroke's record holds the brush settings and its sampled path as
//! points on faces of its input revision (MF-HIST-002), never raw
//! positions. Executing the command replays the samples through the same
//! [`StrokeState`] the live preview uses, so the committed stroke, the
//! preview and a replay are identical.

mod brush;
mod falloff;
mod index;
mod remesh;
mod stroke;

pub use brush::{BrushKind, BrushParams, DabSample};
pub use falloff::Falloff;
pub use index::SculptIndex;
pub use remesh::{RegionAdjacency, RemeshStats, remesh_band, remesh_region};
pub use stroke::{DabDelta, StrokeState};

use mf_geometry::{FaceId, Mesh};
use mf_spec::Params;

use crate::affine::{apply, apply_linear, determinant, inverse};
use crate::{
    DocOutput, ObjectChange, OpError, OpOutput, OperandInput, Operation, RunContext, Target,
    needs_document, unsupported_target,
};

/// The progress stages of `mesh.sculpt_stroke`.
pub const INDEXING: &str = "finding the faces";
pub const STROKING: &str = "applying the stroke";

pub struct SculptStroke;

fn failed(code: &'static str, message: impl Into<String>) -> OpError {
    OpError::Failed {
        code,
        message: message.into(),
    }
}

/// The stroke's brush settings in the object's own space, from the
/// recorded world-space parameters and the object's transform.
///
/// - The radius is divided by the transform's mean scale (cube root of
///   the determinant's magnitude).
/// - The pivot goes through the inverse transform; the view direction
///   through its linear part.
/// - Symmetry planes are normal to the **object's** axes, through the
///   pivot: a model's own symmetry, wherever it's placed.
pub fn brush_params(
    params: &Params,
    transform: &[f64; 12],
    target: &Target,
) -> Result<BrushParams, OpError> {
    let inv = inverse(transform)
        .ok_or_else(|| failed("singular_transform", "the object's transform flattens it"))?;
    let kind = BrushKind::parse(params.get_choice("brush")?)
        .ok_or_else(|| failed("bad_brush", "unknown brush"))?;
    let falloff = Falloff::parse(params.get_choice("falloff")?)
        .ok_or_else(|| failed("bad_falloff", "unknown falloff"))?;
    let mean_scale = determinant(transform).abs().cbrt();
    let radius = params.get_float("radius")? / mean_scale;
    if !(radius > 0.0 && radius.is_finite()) {
        return Err(failed(
            "bad_radius",
            "The brush radius must be more than zero.",
        ));
    }
    let limit_to = match target {
        Target::WholeObject => None,
        Target::Faces(f) => Some(f.clone()),
        other => return Err(unsupported_target("mesh.sculpt_stroke", other)),
    };
    Ok(BrushParams {
        kind,
        radius,
        strength: params.get_float("strength")?,
        falloff,
        symmetry: [
            params.get_bool("symmetry_x")?,
            params.get_bool("symmetry_y")?,
            params.get_bool("symmetry_z")?,
        ],
        pivot: apply(&inv, params.get_vec3("pivot")?),
        limit_to,
        frozen: params
            .get_faces("frozen")?
            .into_iter()
            .map(FaceId)
            .collect(),
        view_dir: apply_linear(&inv, params.get_vec3("view_dir")?),
        detail: match params.get_choice("detail_mode")? {
            "off" => 0.0,
            _ => params.get_float("detail")?,
        },
    })
}

/// Runs a whole stroke on `mesh`: what the command does, and what a live
/// stroke ends as.
pub fn run_stroke(
    mesh: &Mesh,
    brush: BrushParams,
    samples: &[DabSample],
    ctx: &RunContext,
) -> Result<(Mesh, mf_geometry::Provenance), OpError> {
    ctx.progress.report(INDEXING, 0.0);
    let index = SculptIndex::build(mesh, ctx)?;
    let mut stroke = StrokeState::new(mesh, &index, brush);
    for s in samples {
        stroke.check_sample(s)?;
    }
    let span = ctx.span(STROKING, 0.0, 1.0, samples.len());
    for (i, s) in samples.iter().enumerate() {
        span.step(i)?;
        stroke.apply(s, ctx)?;
    }
    span.finish();
    stroke.finish()
}

impl Operation for SculptStroke {
    fn id(&self) -> &'static str {
        "mesh.sculpt_stroke"
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
        let target = inputs.first().ok_or_else(|| {
            failed(
                "missing_target",
                "mesh.sculpt_stroke was run with no target",
            )
        })?;
        let brush = brush_params(params, &target.transform, &target.target)?;
        let samples: Vec<DabSample> = params
            .get_samples("samples")?
            .into_iter()
            .map(DabSample::from_array)
            .collect();
        if samples.is_empty() {
            return Ok(DocOutput {
                changes: Vec::new(),
                report: serde_json::json!({ "dabs": 0 }),
            });
        }
        let (mesh, provenance) = run_stroke(target.mesh, brush, &samples, ctx)?;
        let report = serde_json::json!({
            "dabs": samples.len(),
            "moved_vertices": provenance.moved_vertices.len(),
            "created_faces": provenance.created_faces.len(),
            "removed_faces": provenance.removed_faces.len(),
        });
        if provenance.is_empty() {
            return Ok(DocOutput {
                changes: Vec::new(),
                report,
            });
        }
        Ok(DocOutput {
            changes: vec![ObjectChange::Geometry {
                operand: 0,
                mesh,
                provenance,
            }],
            report,
        })
    }
}
