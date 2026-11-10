//! Object-level operations (MF-OBJ-001): `object.set_transform`,
//! `object.drop_to_plate` and `object.delete`. They change an object's
//! placement or remove it, never its geometry, so they implement
//! [`Operation::execute_doc`] and refuse the single-mesh `execute`.
//!
//! Transforms are the absolute 3MF row-major 3×4 affine (D7, see
//! `mf_render::model_matrix` and `mf_io::transform_mesh`): a point `p`
//! goes to `p * M`, so world z is `m02 x + m12 y + m22 z + m32`.

use mf_geometry::Mesh;
use mf_spec::Params;

use crate::{
    DocOutput, ObjectChange, OpError, OpOutput, OperandInput, Operation, RunContext, Target,
    needs_document, unsupported_target,
};

/// The progress stage of [`world_min_z_with`].
pub const FINDING_LOWEST_POINT: &str = "finding the lowest point";

/// The lowest world-space z of `mesh`'s live vertices under `transform`,
/// or `None` for an empty mesh.
pub fn world_min_z(mesh: &Mesh, transform: &[f64; 12]) -> Option<f64> {
    world_min_z_with(mesh, transform, &RunContext::none())
        .expect("a context that is never cancelled can't be cancelled")
}

/// As [`world_min_z`], cancellable, reporting [`FINDING_LOWEST_POINT`]
/// progress. O(vertices).
pub fn world_min_z_with(
    mesh: &Mesh,
    transform: &[f64; 12],
    ctx: &RunContext,
) -> Result<Option<f64>, crate::Cancelled> {
    Ok(lowest_point(mesh, transform, ctx)?.map(|(z, _)| z))
}

/// The lowest world z, and the largest magnitude of a term summed to
/// compute any world z (the scale of its rounding error).
fn lowest_point(
    mesh: &Mesh,
    transform: &[f64; 12],
    ctx: &RunContext,
) -> Result<Option<(f64, f64)>, crate::Cancelled> {
    let t = transform;
    let span = ctx.span(FINDING_LOWEST_POINT, 0.0, 1.0, mesh.vertex_count());
    let mut min: Option<f64> = None;
    let mut scale = t[11].abs();
    for (n, (_, p)) in mesh.vertices().enumerate() {
        span.at(n)?;
        let terms = [t[2] * p[0], t[5] * p[1], t[8] * p[2]];
        let z = terms[0] + terms[1] + terms[2] + t[11];
        min = Some(min.map_or(z, |m| m.min(z)));
        scale = terms.iter().fold(scale, |s, x| s.max(x.abs()));
    }
    ctx.checkpoint()?;
    span.finish();
    Ok(min.map(|z| (z, scale)))
}

/// Determinant of the linear part (3MF's rows are the column-vector
/// matrix's columns; the determinant is the same).
fn determinant(t: &[f64; 12]) -> f64 {
    t[0] * (t[4] * t[8] - t[5] * t[7]) - t[1] * (t[3] * t[8] - t[5] * t[6])
        + t[2] * (t[3] * t[7] - t[4] * t[6])
}

fn whole_object<'a>(
    op: &str,
    inputs: &'a [OperandInput<'a>],
) -> Result<&'a OperandInput<'a>, OpError> {
    let target = inputs.first().ok_or_else(|| OpError::Failed {
        code: "missing_target",
        message: format!("{op} was run with no target"),
    })?;
    if target.target != Target::WholeObject {
        return Err(unsupported_target(op, &target.target));
    }
    Ok(target)
}

/// The change to `transform`, or none if it's bit-for-bit what the object
/// already has.
fn transform_change(input: &OperandInput, transform: [f64; 12]) -> Vec<ObjectChange> {
    if transform.map(f64::to_bits) == input.transform.map(f64::to_bits) {
        Vec::new()
    } else {
        vec![ObjectChange::Transform {
            operand: 0,
            transform,
        }]
    }
}

pub struct SetTransform;

impl Operation for SetTransform {
    fn id(&self) -> &'static str {
        "object.set_transform"
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
        let target = whole_object(self.id(), inputs)?;
        let transform = params.get_affine("transform")?;
        ctx.checkpoint()?;
        // A singular transform flattens the object to a plane, a line or a
        // point, and no later edit could undo that from the placement.
        let det = determinant(&transform);
        if det == 0.0 || !det.is_finite() {
            return Err(OpError::Failed {
                code: "singular_transform",
                message: format!(
                    "the transform's linear part has determinant {det}, which would flatten the \
                     object; use a non-zero scale on every axis"
                ),
            });
        }
        Ok(DocOutput {
            changes: transform_change(target, transform),
            report: serde_json::json!({ "transform": transform }),
        })
    }
}

pub struct DropToPlate;

impl Operation for DropToPlate {
    fn id(&self) -> &'static str {
        "object.drop_to_plate"
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
        _params: &Params,
        ctx: &RunContext,
    ) -> Result<DocOutput, OpError> {
        let target = whole_object(self.id(), inputs)?;
        ctx.checkpoint()?;
        let (min_z, scale) =
            lowest_point(target.mesh, &target.transform, ctx)?.ok_or_else(|| OpError::Failed {
                code: "empty_object",
                message: "the object has no geometry to rest on the plate".into(),
            })?;
        // Already resting: within the rounding of computing world z at
        // all (a few ulps of its largest term), so dropping twice is one
        // change, not a drift of ulps.
        let resting = min_z.abs() <= 4.0 * f64::EPSILON * scale;
        let mut transform = target.transform;
        if !resting {
            transform[11] -= min_z;
        }
        Ok(DocOutput {
            changes: transform_change(target, transform),
            report: serde_json::json!({ "moved_z": -min_z }),
        })
    }
}

pub struct Delete;

impl Operation for Delete {
    fn id(&self) -> &'static str {
        "object.delete"
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
        _params: &Params,
        ctx: &RunContext,
    ) -> Result<DocOutput, OpError> {
        let target = whole_object(self.id(), inputs)?;
        ctx.checkpoint()?;
        Ok(DocOutput {
            changes: vec![ObjectChange::Remove { operand: 0 }],
            report: serde_json::json!({ "removed_faces": target.mesh.face_count() }),
        })
    }
}
