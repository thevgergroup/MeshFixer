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
    DocContext, DocOutput, ObjectChange, OpError, OpOutput, OperandInput, Operation, RunContext,
    Target, needs_document, unsupported_target,
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

/// The lowest world z, and `S`: the largest, over vertices, of the sum of
/// the absolute values of the four terms summed to compute a world z
/// (`|m02 x| + |m12 y| + |m22 z| + |m32|`), the scale of its rounding
/// error (see [`resting_tolerance`]).
fn lowest_point(
    mesh: &Mesh,
    transform: &[f64; 12],
    ctx: &RunContext,
) -> Result<Option<(f64, f64)>, crate::Cancelled> {
    let t = transform;
    let span = ctx.span(FINDING_LOWEST_POINT, 0.0, 1.0, mesh.vertex_count());
    let mut min: Option<f64> = None;
    let mut scale = 0.0_f64;
    for (n, (_, p)) in mesh.vertices().enumerate() {
        span.at(n)?;
        let terms = [t[2] * p[0], t[5] * p[1], t[8] * p[2]];
        let z = terms[0] + terms[1] + terms[2] + t[11];
        min = Some(min.map_or(z, |m| m.min(z)));
        scale = scale.max(terms.iter().map(|x| x.abs()).sum::<f64>() + t[11].abs());
    }
    ctx.checkpoint()?;
    span.finish();
    Ok(min.map(|z| (z, scale)))
}

/// How far from zero a computed lowest z may be and still mean "resting
/// on the plate", for a transform whose world-z terms have scale `S` (see
/// [`lowest_point`]).
///
/// Derivation (Higham, *Accuracy and Stability*, §3.1 and §3.5; `u =
/// ε/2`, `γₙ = n·u / (1 − n·u)`):
/// - A computed world z is a 4-term dot product (`t[11]` times an exact 1),
///   so it is within `γ₄·S` of the exact value. A computed minimum within
///   `γ₄·S` of zero is therefore indistinguishable from zero.
/// - A drop with scale `S₀` sets `t' = fl(t − m)`. The exact lowest z
///   afterwards is `(min w − m) + δ(t − m)` with `|δ| ≤ u`, so at most
///   `γ₄·S₀ + u·(2 + γ₄)·S₀ ≤ γ₄·S₀ + 2.01·u·S₀`.
/// - After a drop the translation is about the object's own extent, so
///   `S₀ ≤ 3·S` for the next call's scale `S`. The next computed minimum is
///   then within `3·(γ₄ + 2.01·u)·S + γ₄·S ≤ 5.6·γ₄·S` of zero (with
///   `u ≤ γ₄/4`).
///
/// `8·γ₄·S` covers that with margin. An object dropped from far above the
/// plate (`S₀ ≫ S`) can be left with a residual larger than this, so
/// `object.drop_to_plate` corrects again on the new transform until this
/// bound holds (review I-B; see [`MAX_CORRECTIONS`]). It ends on a
/// transform whose computed lowest point is within the bound, so a second
/// drop computes the same value and changes nothing, by construction.
fn resting_tolerance(scale: f64) -> f64 {
    let u = f64::EPSILON / 2.0;
    let gamma4 = 4.0 * u / (1.0 - 4.0 * u);
    8.0 * gamma4 * scale
}

/// Most corrections `object.drop_to_plate` makes before refusing. Each
/// one shrinks the residual from `≈ γ₄·S₀` to `≈ γ₄·S` (the bound above);
/// from any finite height it rests after two or three.
pub const MAX_CORRECTIONS: usize = 8;

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
        let target = whole_object(self.id(), inputs)?;
        let transform = params.get_affine("transform")?;
        ctx.checkpoint()?;
        // A singular transform flattens the object to a plane, a line or a
        // point, and no later edit could undo that from the placement.
        let det = mf_geometry::affine::determinant(&transform);
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
        _: &DocContext,
        _: &RunContext,
    ) -> Result<OpOutput, OpError> {
        Err(needs_document(self.id()))
    }

    fn execute_doc(
        &self,
        inputs: &[OperandInput],
        _params: &Params,
        _doc: &DocContext,
        ctx: &RunContext,
    ) -> Result<DocOutput, OpError> {
        let target = whole_object(self.id(), inputs)?;
        ctx.checkpoint()?;
        let (min_z, scale) =
            lowest_point(target.mesh, &target.transform, ctx)?.ok_or_else(|| OpError::Failed {
                code: "empty_object",
                message: "the object has no geometry to rest on the plate".into(),
            })?;
        let non_finite = || OpError::Failed {
            code: "non_finite_position",
            message: "the object's lowest point is out of floating-point range (a vertex or the \
                      transform is too large); reset its transform, or scale it down first"
                .into(),
        };
        if !min_z.is_finite() || !scale.is_finite() {
            return Err(non_finite());
        }
        // Lower (or raise) by the computed lowest point, then measure again
        // on the new transform, until it rests within the rounding of
        // computing world z at all (see `resting_tolerance`). The result is
        // a transform this same computation calls resting, so dropping it
        // again changes nothing.
        let mut transform = target.transform;
        let (mut min_z, mut scale) = (min_z, scale);
        let mut corrections = 0;
        while min_z.abs() > resting_tolerance(scale) {
            if corrections == MAX_CORRECTIONS {
                return Err(OpError::Failed {
                    code: "did_not_settle",
                    message: format!(
                        "the object didn't come to rest on the plate after {MAX_CORRECTIONS} \
                         corrections (still {min_z} off); reset its transform and drop it again"
                    ),
                });
            }
            transform[11] -= min_z;
            if !transform[11].is_finite() {
                return Err(non_finite());
            }
            (min_z, scale) = lowest_point(target.mesh, &transform, ctx)?
                .expect("the mesh had vertices a moment ago");
            if !min_z.is_finite() || !scale.is_finite() {
                return Err(non_finite());
            }
            corrections += 1;
        }
        let moved_z = transform[11] - target.transform[11];
        Ok(DocOutput {
            changes: transform_change(target, transform),
            report: serde_json::json!({ "moved_z": moved_z }),
        })
    }
}

/// `object.rename`: gives the target a new name (MF-OBJ-001). Geometry and
/// transform are unchanged and stay shared; replay matches Exact because
/// the object's content hash never depends on its name.
pub struct Rename;

impl Operation for Rename {
    fn id(&self) -> &'static str {
        "object.rename"
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
        let target = whole_object(self.id(), inputs)?;
        let name = params.get_text("name")?;
        ctx.checkpoint()?;
        if name.is_empty() {
            return Err(OpError::Failed {
                code: "empty_name",
                message: "an object's name can't be empty".into(),
            });
        }
        let changes = if name == target.name {
            Vec::new()
        } else {
            vec![ObjectChange::Rename {
                operand: 0,
                name: name.to_owned(),
            }]
        };
        Ok(DocOutput {
            changes,
            report: serde_json::json!({ "name": name }),
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
        _: &DocContext,
        _: &RunContext,
    ) -> Result<OpOutput, OpError> {
        Err(needs_document(self.id()))
    }

    fn execute_doc(
        &self,
        inputs: &[OperandInput],
        _params: &Params,
        _doc: &DocContext,
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

/// `object.arrange_on_plate` (D21): moves the object to the first free
/// spot on the build plate, clear of every other object (the scene), and
/// rests it on the plate. Only its x/y/z translation changes.
pub struct ArrangeOnPlate;

impl Operation for ArrangeOnPlate {
    fn id(&self) -> &'static str {
        "object.arrange_on_plate"
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

    fn wants_scene(&self) -> bool {
        true
    }

    fn execute_doc(
        &self,
        inputs: &[OperandInput],
        params: &Params,
        doc: &DocContext,
        ctx: &RunContext,
    ) -> Result<DocOutput, OpError> {
        self.execute_scene(inputs, &[], params, doc, ctx)
    }

    fn execute_scene(
        &self,
        inputs: &[OperandInput],
        scene: &[crate::SceneObject],
        params: &Params,
        doc: &DocContext,
        ctx: &RunContext,
    ) -> Result<DocOutput, OpError> {
        let target = whole_object(self.id(), inputs)?;
        let empty = || OpError::Failed {
            code: "empty_object",
            message: "the object has no geometry to place on the plate".into(),
        };
        let own = crate::SceneObject::of(target.object, target.mesh, &target.transform, ctx)?
            .ok_or_else(empty)?;
        let occupied: Vec<crate::layout::Footprint> = scene
            .iter()
            .filter(|o| o.object != target.object)
            .map(|o| o.footprint())
            .collect();
        let plate = crate::layout::Plate {
            size: [
                params.get_float("plate_width")?,
                params.get_float("plate_depth")?,
            ],
        };
        let layout = crate::layout::layout_on_plate(
            &occupied,
            &[own.footprint()],
            &plate,
            params.get_float("spacing")?,
        );
        let mut moved = target.transform;
        moved[9] += layout.transforms[0][9];
        moved[10] += layout.transforms[0][10];
        // Then rest it on the plate, exactly as drop_to_plate does.
        let dropped = DropToPlate.execute_doc(
            &[OperandInput {
                slot: "target",
                transform: moved,
                ..target.clone()
            }],
            params,
            doc,
            ctx,
        )?;
        let transform = dropped
            .changes
            .iter()
            .find_map(|c| match c {
                ObjectChange::Transform { transform, .. } => Some(*transform),
                _ => None,
            })
            .unwrap_or(moved); // allow-swallow: no drop change means it already rests at the new spot
        let off_plate = !layout.off_plate.is_empty();
        let changes = if transform.map(f64::to_bits) == target.transform.map(f64::to_bits) {
            Vec::new()
        } else {
            vec![ObjectChange::Transform {
                operand: 0,
                transform,
            }]
        };
        Ok(DocOutput {
            changes,
            report: serde_json::json!({
                "transform": transform,
                "off_plate": off_plate,
                "message": off_plate.then_some("The plate is full; the object is placed beside it"),
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests may panic on failure

    use super::*;
    use mf_document::ObjectId;

    fn drop(mesh: &Mesh, transform: [f64; 12]) -> Result<DocOutput, OpError> {
        DropToPlate.execute_doc(
            &[OperandInput {
                slot: "target",
                object: ObjectId(1),
                name: "object",
                mesh,
                transform,
                target: Target::WholeObject,
            }],
            &Params::default(),
            &DocContext::default(),
            &RunContext::none(),
        )
    }

    fn dropped(out: &DocOutput, t: [f64; 12]) -> [f64; 12] {
        match out.changes.as_slice() {
            [ObjectChange::Transform { transform, .. }] => *transform,
            [] => t,
            other => panic!("{other:?}"),
        }
    }

    /// Rotated, scaled and translated meshes: a second drop never moves
    /// the object again (the "resting" bound holds), and a resting object
    /// reports `moved_z: 0`.
    #[test]
    fn dropping_twice_changes_nothing_the_second_time() {
        let mesh = Mesh::new(
            vec![
                [0.3, -1.7, 2.9],
                [4.1, 0.2, -0.6],
                [-2.2, 3.3, 1.1],
                [0.0, 0.0, -5.5],
            ],
            vec![[0, 1, 2], [0, 2, 3], [0, 3, 1], [1, 3, 2]],
        )
        .unwrap();
        let mut seed = 0x1234_5678_u64;
        let mut next = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 11) as f64 / (1u64 << 53) as f64
        };
        for _ in 0..500 {
            let (a, b) = (next() * 6.3, next() * 6.3);
            let k = 0.1 + next() * 10.0;
            let (sa, ca) = a.sin_cos();
            let (sb, cb) = b.sin_cos();
            // Rotation about x then z, scaled; translation within the
            // object's own extent (as after a first drop).
            let t = [
                k * cb,
                k * sb * ca,
                k * sb * sa,
                -k * sb,
                k * cb * ca,
                k * cb * sa,
                0.0,
                -k * sa,
                k * ca,
                next() * 20.0 - 10.0,
                next() * 20.0 - 10.0,
                (next() * 20.0 - 10.0) * k,
            ];
            let once = dropped(&drop(&mesh, t).unwrap(), t);
            let second = drop(&mesh, once).unwrap();
            assert!(second.changes.is_empty(), "{t:?}: {:?}", second.changes);
            assert_eq!(second.report["moved_z"], serde_json::json!(0.0));
        }
    }

    /// Review I-B: one drop from far above the plate (1e3 and 1e6 mm, on
    /// 2 mm and 20 mm parts, at random rotations) comes to rest; a second
    /// drop changes nothing.
    #[test]
    fn one_drop_from_far_above_rests() {
        let unit = Mesh::new(
            vec![
                [0.3, -0.7, 0.9],
                [1.0, 0.2, -0.6],
                [-0.8, 0.9, 0.1],
                [0.0, 0.0, -1.0],
            ],
            vec![[0, 1, 2], [0, 2, 3], [0, 3, 1], [1, 3, 2]],
        )
        .unwrap();
        let mut seed = 0xfeed_u64;
        let mut next = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 11) as f64 / (1u64 << 53) as f64
        };
        for size in [2.0, 20.0] {
            for height in [1e3, 1e6] {
                for _ in 0..500 {
                    let (a, b) = (next() * 6.3, next() * 6.3);
                    let k = size * (0.5 + next());
                    let (sa, ca) = a.sin_cos();
                    let (sb, cb) = b.sin_cos();
                    let t = [
                        k * cb,
                        k * sb * ca,
                        k * sb * sa,
                        -k * sb,
                        k * cb * ca,
                        k * cb * sa,
                        0.0,
                        -k * sa,
                        k * ca,
                        next() * 100.0,
                        next() * 100.0,
                        height * (1.0 + next()),
                    ];
                    let once = drop(&unit, t).unwrap();
                    assert!(
                        (once.report["moved_z"].as_f64().unwrap() + height).abs() < 2.0 * height
                    );
                    let rested = dropped(&once, t);
                    let second = drop(&unit, rested).unwrap();
                    assert!(
                        second.changes.is_empty(),
                        "size {size} height {height} {t:?}: {:?}",
                        second.changes
                    );
                }
            }
        }
    }

    #[test]
    fn a_non_finite_result_is_refused_by_name() {
        let mesh = Mesh::new(
            vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 1e300]],
            vec![[0, 1, 2]],
        )
        .unwrap();
        let mut t = mf_io::threemf::IDENTITY;
        t[8] = 1e10;
        match drop(&mesh, t) {
            Err(OpError::Failed { code, .. }) => assert_eq!(code, "non_finite_position"),
            other => panic!("{:?}", other.map(|o| o.changes)),
        }
    }
}
