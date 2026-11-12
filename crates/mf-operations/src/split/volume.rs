//! `mesh.split_box` (the Split workflow's Print-volume mode, owner
//! 2026-11-11): cut every selected object along the faces of a world
//! axis-aligned box (a printer's build volume), so the piece inside the
//! box fits the printer, and cap every cut flat.
//!
//! - The box's six faces are cut in a fixed order (−X, +X, −Y, +Y, −Z,
//!   +Z), each through the piece still inside: the part beyond the face
//!   becomes its own object ("scan (outside +X)") and isn't cut again; a
//!   face the inside piece doesn't reach is skipped. The inside piece
//!   keeps the target's id ("scan (in box)").
//! - Every cut is `mf_boolean::cut_half` with its cap, exactly as
//!   `mesh.split_plane`; `cut_gap` removes a slab of that thickness
//!   centred on each face.
//! - Each cut is recorded in the report as a **joint**: the inside piece
//!   (`a`, on the box's side of the face) and the outside piece (`b`),
//!   the face's centre and outward normal (`a` → `b`) and the gap, so
//!   the Split panel's step 3 can place connectors across it.
//! - A target the box doesn't reach at all is listed in `missed`; one
//!   wholly inside the box is listed in `inside` (nothing to cut);
//!   refused only when nothing at all is cut.

use mf_boolean::{CutError, Plane, Side};
use mf_geometry::Mesh;
use mf_spec::Params;

use super::{as_created, failed, normal_to_object, refusal};
use crate::affine::{apply, inverse};
use crate::{
    DocOutput, ObjectChange, OpError, OpOutput, OperandInput, Operation, RunContext, Target,
    needs_document, unsupported_target,
};

pub struct SplitBox;

/// The box's faces, in the order they're cut: (axis, sign, label).
pub const BOX_FACES: [(usize, f64, &str); 6] = [
    (0, -1.0, "-X"),
    (0, 1.0, "+X"),
    (1, -1.0, "-Y"),
    (1, 1.0, "+Y"),
    (2, -1.0, "-Z"),
    (2, 1.0, "+Z"),
];

/// The names a box split gives its pieces: the inside one, and the one
/// outside face `label`.
pub fn inside_name(name: &str) -> String {
    format!("{name} (in box)")
}

pub fn outside_name(name: &str, label: &str) -> String {
    format!("{name} (outside {label})")
}

/// Where `mesh` (placed by `t`) lies against the world plane `(p, n)`:
/// `(min, max)` signed distance over its vertices.
fn extent(mesh: &Mesh, t: &[f64; 12], p: [f64; 3], n: [f64; 3]) -> (f64, f64) {
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for (_, v) in mesh.vertices() {
        let w = apply(t, v);
        let d = (w[0] - p[0]) * n[0] + (w[1] - p[1]) * n[1] + (w[2] - p[2]) * n[2];
        lo = lo.min(d);
        hi = hi.max(d);
    }
    (lo, hi)
}

impl Operation for SplitBox {
    fn id(&self) -> &'static str {
        "mesh.split_box"
    }

    fn version(&self) -> u32 {
        2
    }

    fn upgrade_params(&self, recorded: u32, params: &mut Params) -> bool {
        match recorded {
            // Version 1 always capped every cut.
            1 => {
                params
                    .0
                    .insert("cap".to_owned(), mf_spec::ParamValue::Bool(true));
                true
            }
            v => v == self.version(),
        }
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
        if inputs.is_empty() {
            return Err(failed(
                "missing_target",
                "mesh.split_box was run with no target",
            ));
        }
        let lo = params.get_vec3("box_min")?;
        let hi = params.get_vec3("box_max")?;
        let gap = params.get_float("cut_gap")?;
        let cap = params.get_bool("cap")?;
        if (0..3).any(|i| hi[i] <= lo[i] || !lo[i].is_finite() || !hi[i].is_finite()) {
            return Err(failed(
                "invalid_box",
                "The box has no size along one of its axes; give it a width, depth and height.",
            ));
        }
        let mut changes = Vec::new();
        let mut created = 0usize;
        let mut pieces = Vec::new();
        let mut joints = Vec::new();
        let mut missed = Vec::new();
        let mut inside = Vec::new();
        for (k, target) in inputs.iter().enumerate() {
            if target.target != Target::WholeObject {
                return Err(unsupported_target(self.id(), &target.target));
            }
            let inv = inverse(&target.transform).ok_or_else(|| {
                failed(
                    "singular_transform",
                    "The object's transform flattens it, so the box can't be placed in it; \
                     reset its transform first.",
                )
            })?;
            mf_boolean::check_closed(target.mesh, ctx)
                .map_err(|e| match refusal(&target.transform, e) {
                    OpError::Failed { code, message } if inputs.len() > 1 => OpError::Failed {
                        code,
                        message: format!("'{}': {message}", target.name),
                    },
                    e => e,
                })?;
            // The piece still inside the box, with its provenance against
            // the target's input.
            let mut piece = target.mesh.clone();
            let mut provenance: Option<mf_geometry::Provenance> = None;
            let mut outside = Vec::new();
            let mut gone = false;
            for (axis, sign, label) in BOX_FACES {
                ctx.checkpoint()?;
                let n: [f64; 3] = std::array::from_fn(|i| if i == axis { sign } else { 0.0 });
                let centre: [f64; 3] = std::array::from_fn(|i| {
                    if i == axis {
                        if sign > 0.0 { hi[i] } else { lo[i] }
                    } else {
                        (lo[i] + hi[i]) / 2.0
                    }
                });
                let (min_d, max_d) = extent(&piece, &target.transform, centre, n);
                if max_d <= -gap / 2.0 {
                    continue; // wholly on the box's side: nothing to cut
                }
                if min_d >= gap / 2.0 {
                    gone = true; // wholly beyond this face: the box misses it
                    break;
                }
                let at = |s: f64| -> [f64; 3] {
                    apply(
                        &inv,
                        std::array::from_fn(|i| centre[i] + s * n[i] * gap / 2.0),
                    )
                };
                let object_n = normal_to_object(&target.transform, n);
                let keep_in = Plane {
                    point: at(-1.0),
                    normal: object_n,
                };
                let keep_out = Plane {
                    point: at(1.0),
                    normal: object_n,
                };
                let inner = mf_boolean::cut_half_with(&piece, &keep_in, Side::Below, cap, ctx);
                let outer = mf_boolean::cut_half_with(&piece, &keep_out, Side::Above, cap, ctx);
                let (inner, outer) = match (inner, outer) {
                    (Ok(i), Ok(o)) => (i, o),
                    // The plane crosses the box around the mesh but not
                    // the mesh itself (or the gap swallows it), or lies
                    // exactly on the piece's own surface there without
                    // anything to cut away: skip.
                    (Err(CutError::MissesObject { .. }), _) | (_, Err(CutError::MissesObject { .. })) => {
                        continue;
                    }
                    (Err(e), _) | (_, Err(e)) => {
                        return Err(match refusal(&target.transform, e) {
                            OpError::Failed { code, message } => OpError::Failed {
                                code,
                                message: format!(
                                    "'{}', at the box's {label} face: {message}",
                                    target.name
                                ),
                            },
                            e => e,
                        });
                    }
                };
                let outer_prov = match &provenance {
                    Some(p) => super::pegs::compose(p, &outer.provenance),
                    None => outer.provenance,
                };
                provenance = Some(match &provenance {
                    Some(p) => super::pegs::compose(p, &inner.provenance),
                    None => inner.provenance,
                });
                piece = inner.mesh;
                outside.push((label, outer.mesh, outer_prov, centre, n));
            }
            if gone {
                missed.push(target.name.to_owned());
                continue;
            }
            let Some(provenance) = provenance else {
                inside.push(target.name.to_owned());
                continue;
            };
            let name = inside_name(target.name);
            pieces.push(piece_report(&name, &piece, &target.transform, k, None));
            changes.push(ObjectChange::Geometry {
                operand: k,
                mesh: piece,
                provenance,
            });
            changes.push(ObjectChange::Rename { operand: k, name });
            for (label, mesh, prov, centre, n) in outside {
                let name = outside_name(target.name, label);
                pieces.push(piece_report(
                    &name,
                    &mesh,
                    &target.transform,
                    k,
                    Some(created),
                ));
                // Uncapped (a vase or bowl), there's no cap face pair to
                // place connectors against, so no joint is recorded.
                if cap {
                    joints.push(serde_json::json!({
                        "a": { "operand": k },
                        "b": { "created": created },
                        "face": label,
                        "point": centre,
                        "normal": n,
                        "gap": gap,
                    }));
                }
                changes.push(ObjectChange::Create {
                    name,
                    mesh,
                    transform: target.transform,
                    provenance: as_created(prov, k),
                });
                created += 1;
            }
        }
        if changes.is_empty() {
            return Err(if !inside.is_empty() && missed.is_empty() {
                failed(
                    "fits_in_box",
                    "The model already fits inside the box, so there's nothing to cut. Make the \
                     box smaller or move it so a face crosses the model.",
                )
            } else {
                failed(
                    "box_misses_object",
                    "The box doesn't reach the model. Move it so the model is at least partly \
                     inside.",
                )
            });
        }
        Ok(DocOutput {
            changes,
            report: serde_json::json!({
                "pieces": pieces,
                "joints": joints,
                "missed": missed,
                "inside": inside,
                "box_min": lo,
                "box_max": hi,
            }),
        })
    }
}

/// One piece's report: its name, which object it is and its world size.
fn piece_report(
    name: &str,
    mesh: &Mesh,
    t: &[f64; 12],
    operand: usize,
    created: Option<usize>,
) -> serde_json::Value {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for (_, v) in mesh.vertices() {
        let w = apply(t, v);
        for i in 0..3 {
            lo[i] = lo[i].min(w[i]);
            hi[i] = hi[i].max(w[i]);
        }
    }
    let size: Vec<f64> = (0..3).map(|i| (hi[i] - lo[i]).max(0.0)).collect();
    serde_json::json!({
        "name": name,
        "object": match created {
            Some(j) => serde_json::json!({ "created": j }),
            None => serde_json::json!({ "operand": operand }),
        },
        "faces": mesh.face_count(),
        "world_min": lo,
        "world_max": hi,
        "size": size,
    })
}
