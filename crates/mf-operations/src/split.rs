//! `mesh.split_plane` (MF-MODEL-003, D11, D23; Phase 1 Task 12): cut a
//! closed object in two along a plane with `mf_boolean::cut_half`, and
//! close each part with a cap.
//!
//! - The plane is given in **world space** (`plane_point` in document
//!   units, converted from mm at prepare time; `plane_normal` a direction)
//!   and brought into the object's space with its recorded transform at
//!   execute time, so replay reads only recorded values.
//! - `cut_gap` (D23) removes a slab of that thickness centred on the
//!   plane: the top part is cut at `plane_point + normal · gap/2`, the
//!   bottom part at `− gap/2`, both measured in world space.
//! - `keep: both`: the top part keeps the target's id (renamed "scan
//!   (top)") and the bottom part is created as "scan (bottom)" with the
//!   same transform. `keep: above` or `below` keeps only that part, in the
//!   target, and the other part's faces are listed as removed.
//! - **The created part is an edit of the target's mesh**, so its faces
//!   away from the plane keep their slot ids and share storage with the
//!   target's history (a 30M split allocates no per-face provenance). Its
//!   provenance is relative to operand 0's input mesh: a face live in both
//!   is that operand face; created faces name their sources in
//!   [`Sources::operands`].
//! - Alignment pegs (`pegs`, Task 13) need the boolean engine.
//!   **HOOK (Task 13):** anything but `pegs: none` is refused as
//!   `pegs_need_booleans` until `split/pegs.rs` lands; the peg parameters
//!   are already in the v1 spec so Task 13 doesn't change the version.

use mf_boolean::{CutError, HalfCut, Plane, Side};
use mf_geometry::{Mesh, Provenance, Sources};
use mf_spec::Params;

use crate::{
    DocOutput, ObjectChange, OpError, OpOutput, OperandInput, Operation, RunContext, Target,
    needs_document, unsupported_target,
};

/// The stages a split reports, in order.
pub const SPLIT_STAGES: &[&str] = &[
    mf_boolean::CHECKING,
    mf_boolean::FINDING_THE_CUT,
    mf_boolean::CUTTING,
    mf_boolean::CAPPING,
];

/// The names a split gives its parts: `(top, bottom)`.
pub fn split_names(name: &str) -> (String, String) {
    (format!("{name} (top)"), format!("{name} (bottom)"))
}

pub struct SplitPlane;

/// Inverse of a 3MF row-vector affine (`p' = p·M + t`), `None` if
/// singular.
fn inverse(t: &[f64; 12]) -> Option<[f64; 12]> {
    let m = [[t[0], t[1], t[2]], [t[3], t[4], t[5]], [t[6], t[7], t[8]]];
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if det == 0.0 || !det.is_finite() {
        return None;
    }
    let cof = |r: usize, c: usize| {
        let (r0, r1) = ((r + 1) % 3, (r + 2) % 3);
        let (c0, c1) = ((c + 1) % 3, (c + 2) % 3);
        m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0]
    };
    let mut inv = [[0.0; 3]; 3];
    for (r, row) in inv.iter_mut().enumerate() {
        for (c, x) in row.iter_mut().enumerate() {
            *x = cof(c, r) / det;
        }
    }
    let tr = [t[9], t[10], t[11]];
    let it: [f64; 3] =
        std::array::from_fn(|c| -(tr[0] * inv[0][c] + tr[1] * inv[1][c] + tr[2] * inv[2][c]));
    Some([
        inv[0][0], inv[0][1], inv[0][2], inv[1][0], inv[1][1], inv[1][2], inv[2][0], inv[2][1],
        inv[2][2], it[0], it[1], it[2],
    ])
}

/// `p·M + t`.
fn apply(t: &[f64; 12], p: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|c| t[c] * p[0] + t[3 + c] * p[1] + t[6 + c] * p[2] + t[9 + c])
}

/// A world normal in object space: with `x_w = x_o·M + t`, the plane
/// `n_w·(x_w − p_w) = 0` is `(M n_w)·(x_o − p_o) = 0`.
fn normal_to_object(t: &[f64; 12], n: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|r| t[3 * r] * n[0] + t[3 * r + 1] * n[1] + t[3 * r + 2] * n[2])
}

fn failed(code: &'static str, message: impl Into<String>) -> OpError {
    OpError::Failed {
        code,
        message: message.into(),
    }
}

/// The refusals, with what to do next (the copy table's plain words).
fn refusal(e: CutError) -> OpError {
    match e {
        CutError::Cancelled => OpError::Cancelled,
        CutError::NotClosed { boundary_edges, .. } => failed(
            "operand_not_closed",
            format!(
                "This model isn't closed ({boundary_edges} open edges), so a cut has no inside \
                 to cap. Close holes first."
            ),
        ),
        CutError::MissesObject => failed(
            "plane_misses_object",
            "The cut plane doesn't pass through the object (or the gap is wider than it). Move \
             the plane so it crosses the model.",
        ),
        CutError::SectionSelfIntersects { at } => failed(
            "section_self_intersects",
            format!(
                "The cut grazes the surface here (near {:.3}, {:.3}, {:.3}); move the plane \
                 slightly.",
                at[0], at[1], at[2]
            ),
        ),
        CutError::InvalidPlane => failed(
            "invalid_plane",
            "The cut plane's direction is zero; give it a direction.",
        ),
        CutError::Geometry(e) => failed("invalid_mesh", e.to_string()),
    }
}

/// A part's provenance as a created object's: its created faces name
/// their sources as faces of operand 0.
fn as_created(mut p: Provenance) -> Provenance {
    for (_, s) in &mut p.created_faces {
        let faces = std::mem::take(&mut s.faces);
        *s = Sources {
            operands: faces.into_iter().map(|f| (0, f)).collect(),
            ..Sources::default()
        };
    }
    p
}

fn part_report(name: &str, object: &str, part: &HalfCut) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "object": object,
        "faces": part.mesh.face_count(),
        "cap_faces": part.cap_faces.iter().map(|f| f.0).collect::<Vec<_>>(),
        "section_regions": part.section_regions.len(),
    })
}

impl Operation for SplitPlane {
    fn id(&self) -> &'static str {
        "mesh.split_plane"
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
        let target: &OperandInput = inputs
            .first()
            .ok_or_else(|| failed("missing_target", "mesh.split_plane was run with no target"))?;
        if target.target != Target::WholeObject {
            return Err(unsupported_target(self.id(), &target.target));
        }
        let point = params.get_vec3("plane_point")?;
        let normal = params.get_vec3("plane_normal")?;
        let keep = params.get_choice("keep")?;
        let gap = params.get_float("cut_gap")?;
        let pegs = params.get_choice("pegs")?;
        if pegs != "none" {
            // HOOK (Task 13): holes, dowels, attached pegs and dovetails.
            return Err(failed(
                "pegs_need_booleans",
                "Alignment holes and pegs need the boolean engine, which this build doesn't have \
                 yet. Split with Alignment: None for now.",
            ));
        }
        let len = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
        if len == 0.0 || !len.is_finite() {
            return Err(refusal(CutError::InvalidPlane));
        }
        let inv = inverse(&target.transform).ok_or_else(|| {
            failed(
                "singular_transform",
                "The object's transform flattens it, so the plane can't be placed in it; reset \
                 its transform first.",
            )
        })?;
        ctx.checkpoint()?;
        // The two cut planes, in world space then object space.
        let unit = normal.map(|c| c / len);
        let shifted = |sign: f64| -> [f64; 3] {
            if gap == 0.0 {
                point
            } else {
                std::array::from_fn(|i| point[i] + sign * unit[i] * gap / 2.0)
            }
        };
        let object_normal = normal_to_object(&target.transform, normal);
        let plane_above = Plane {
            point: apply(&inv, shifted(1.0)),
            normal: object_normal,
        };
        let plane_below = Plane {
            point: apply(&inv, shifted(-1.0)),
            normal: object_normal,
        };
        mf_boolean::check_closed(target.mesh, ctx).map_err(refusal)?;
        let above = (keep != "below")
            .then(|| mf_boolean::cut_half(target.mesh, &plane_above, Side::Above, ctx))
            .transpose()
            .map_err(refusal)?;
        let below = (keep != "above")
            .then(|| mf_boolean::cut_half(target.mesh, &plane_below, Side::Below, ctx))
            .transpose()
            .map_err(refusal)?;
        ctx.checkpoint()?;
        let (top_name, bottom_name) = split_names(target.name);
        let mut changes = Vec::new();
        let mut parts = Vec::new();
        let regions = below
            .as_ref()
            .or(above.as_ref())
            .map_or(0, |p| p.section_regions.len());
        match (above, below) {
            (Some(above), Some(below)) => {
                parts.push(part_report(&top_name, "target", &above));
                parts.push(part_report(&bottom_name, "created", &below));
                changes.push(ObjectChange::Geometry {
                    operand: 0,
                    mesh: above.mesh,
                    provenance: above.provenance,
                });
                changes.push(ObjectChange::Rename {
                    operand: 0,
                    name: top_name,
                });
                changes.push(ObjectChange::Create {
                    name: bottom_name,
                    mesh: below.mesh,
                    transform: target.transform,
                    provenance: as_created(below.provenance),
                });
            }
            (Some(part), None) | (None, Some(part)) => {
                let name = if keep == "above" {
                    top_name
                } else {
                    bottom_name
                };
                parts.push(part_report(&name, "target", &part));
                changes.push(ObjectChange::Geometry {
                    operand: 0,
                    mesh: part.mesh,
                    provenance: part.provenance,
                });
                changes.push(ObjectChange::Rename { operand: 0, name });
            }
            (None, None) => unreachable!("keep is both, above or below"),
        }
        Ok(DocOutput {
            changes,
            report: serde_json::json!({
                "parts": parts,
                "section_regions": regions,
                "plane_point_object": plane_above.point,
                "plane_normal_object": object_normal,
            }),
        })
    }
}
