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
//! - Alignment (`pegs`, Task 13, D12; see [`pegs`]): `dowels` subtracts
//!   holes from both cut faces at the same places and creates each dowel
//!   as its own object ("scan dowel 1"…), upright, laid out on the plate
//!   by `layout_on_plate` (D21) with world min z = 0; `attached` unions
//!   pegs onto the top part and subtracts the holes from the bottom one.
//!   `dovetail` is Task 14 and is refused by name for now.

use glam::DMat4;
use mf_boolean::{CutError, HalfCut, Plane, Side};
use mf_geometry::{Bvh, Mesh, Provenance, Sources};
use mf_parts::{Compensation, FitClass, FitExpectation, MachineProfile, MatingSurfaces, check_fit};
use mf_spec::Params;

use crate::affine::{apply, apply_linear, inverse};
use crate::layout::{Footprint, Plate, layout_on_plate};

pub mod dovetail;
pub mod pegs;
pub mod region;
use crate::{
    DocOutput, ObjectChange, OpError, OpOutput, OperandInput, Operation, RunContext, Target,
    needs_document, unsupported_target,
};
use pegs::{PegError, PegShape};

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

    fn wants_scene(&self) -> bool {
        true
    }

    fn execute_doc(
        &self,
        inputs: &[OperandInput],
        params: &Params,
        ctx: &RunContext,
    ) -> Result<DocOutput, OpError> {
        self.execute_scene(inputs, &[], params, ctx)
    }

    fn execute_scene(
        &self,
        inputs: &[OperandInput],
        scene: &[crate::SceneObject],
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
        let mut above = above;
        let mut below = below;
        let mut created_dowels = Vec::new();
        let mut peg_report = serde_json::Value::Null;
        if pegs != "none" {
            let world_n = unit;
            let out = alignment(
                params, pegs, target, scene, world_n, &mut above, &mut below, ctx,
            )?;
            created_dowels = out.0;
            peg_report = out.1;
        }
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
        for (k, (mesh, transform)) in created_dowels.into_iter().enumerate() {
            let provenance = Provenance {
                created_faces: mesh.faces().map(|(f, _)| (f, Sources::default())).collect(),
                created_vertices: mesh.vertices().map(|(v, _)| v).collect(),
                ..Provenance::default()
            };
            changes.push(ObjectChange::Create {
                name: format!("{} dowel {}", target.name, k + 1),
                mesh,
                transform,
                provenance,
            });
        }
        Ok(DocOutput {
            changes,
            report: serde_json::json!({
                "parts": parts,
                "section_regions": regions,
                "plane_point_object": plane_above.point,
                "plane_normal_object": object_normal,
                "pegs": peg_report,
            }),
        })
    }
}

/// The world footprint of `mesh` placed by `t` (its bounding box's
/// corners, transformed).
fn footprint(mesh: &Mesh, t: &[f64; 12]) -> Option<Footprint> {
    let bb = mesh.bounding_box()?;
    let mut f = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for c in 0..8 {
        let p = [0, 1, 2].map(|a| {
            if c & (1 << a) != 0 {
                bb.max[a]
            } else {
                bb.min[a]
            }
        });
        let w = apply(t, p);
        f = [
            f[0].min(w[0]),
            f[1].min(w[1]),
            f[2].max(w[0]),
            f[3].max(w[1]),
        ];
    }
    Some(f)
}

/// A new object's mesh and placement.
type Placed = (Mesh, [f64; 12]);

/// Holes, attached pegs and dowels (D12). Changes the parts in place
/// (their provenance stays relative to the target's input) and returns the
/// dowels to create, each with its placement, and a report.
#[allow(clippy::too_many_arguments)]
fn alignment(
    params: &Params,
    pegs: &str,
    target: &OperandInput,
    scene: &[crate::SceneObject],
    world_n: [f64; 3],
    above: &mut Option<HalfCut>,
    below: &mut Option<HalfCut>,
    ctx: &RunContext,
) -> Result<(Vec<Placed>, serde_json::Value), OpError> {
    let shape = match params.get_choice("peg_shape")? {
        "square" => PegShape::Square,
        "keyed" => PegShape::Keyed,
        _ => PegShape::Round,
    };
    let d = params.get_float("hole_diameter")?;
    let depth_above = params.get_float("hole_depth_above")?;
    let depth_below = params.get_float("hole_depth_below")?;
    let count = params.get_int("peg_count")?.max(0) as u32;
    // D20 migration: `peg_clearance` is what sizes the hole and dowel
    // (unchanged, so an old log that only recorded `peg_clearance`
    // replays identically). `fit_clearance` and `machine_tolerance` are
    // the new, separately recorded breakdown (Fit class clearance plus
    // Machine profile hole-undersize compensation); a frontend on the
    // new path fills `peg_clearance` from their sum before this runs, so
    // execution always reads one number but the record keeps both.
    let clearance = params.get_float("peg_clearance")?;
    let fit_clearance = params.get_float("fit_clearance")?;
    let machine_tolerance = params.get_float("machine_tolerance")?;
    let allowance = params.get_float("length_allowance")?;
    let min_wall = params.get_float("min_wall")?;
    let plate = Plate {
        size: [
            params.get_float("plate_width")?,
            params.get_float("plate_depth")?,
        ],
    };
    let spacing = params.get_float("plate_spacing")?;
    let (dowel_d, dowel_len) = pegs::dowel_size(d, depth_above, depth_below, clearance, allowance);
    let attached_len = depth_below - allowance;
    if (pegs == "dowels" && dowel_len <= 0.0) || (pegs == "attached" && attached_len <= 0.0) {
        return Err(failed(
            "dowel_too_short",
            "The holes are too shallow for a dowel after the length allowance. Make the holes \
             deeper.",
        ));
    }
    if dowel_d <= 0.0 || d <= 0.0 {
        return Err(failed(
            "dowel_too_thin",
            "The hole is no wider than the clearance, so the dowel would have no width. Make \
             the hole wider.",
        ));
    }
    let chord_tol = if clearance > 0.0 {
        clearance / 10.0
    } else {
        d / 1000.0
    };
    // Object units per world unit (uniform-scale approximation): sizes
    // compared with the section are in object units.
    let scale = crate::affine::determinant(&target.transform).abs().cbrt();
    let scale = if scale > 0.0 && scale.is_finite() {
        scale
    } else {
        1.0
    };
    // Placement on one section (the bottom part's, or the top's if only
    // it is kept).
    let regions = below
        .as_ref()
        .or(above.as_ref())
        .map(|p| p.section_regions.clone())
        .unwrap_or_default(); // allow-swallow: no part kept on either side means no section, so no holes
    let dovetail = pegs == "dovetail";
    let (shape, count) = if dovetail {
        (PegShape::Round, 1)
    } else {
        (shape, count)
    };
    let (holes, skipped) = pegs::place_holes(&regions, shape, d / scale, count, min_wall / scale)
        .map_err(|e| match e {
        PegError::DoesNotFit {
            region,
            widest_fit_mm,
        } => failed(
            "section_too_thin",
            format!(
                "Section {} is too thin for a {d:.2} hole with a {min_wall:.2} wall: the \
                     widest hole that fits is {:.2}. Use a smaller hole diameter or minimum wall, \
                     or Alignment: None.",
                region + 1,
                widest_fit_mm * scale
            ),
        ),
    })?;
    ctx.checkpoint()?;
    let over = d + params.get_float("cut_gap")?;
    // World base points (on the bottom plane, and the top plane) and keys.
    let frame_below = regions.first().map(|r| r.frame);
    let frame_above = above
        .as_ref()
        .and_then(|p| p.section_regions.first())
        .map(|r| r.frame);
    let mut tools_below = Vec::new();
    let mut tools_above = Vec::new();
    let mut pegs_above = Vec::new();
    // The first hole's mating pair, in world space, for the exact fit
    // check below: (hole tool mesh, dowel-or-peg mesh at the hole's own
    // clearance). Captured before the boolean consumes the tool meshes.
    let mut fit_pair: Option<(Mesh, Mesh)> = None;
    for h in &holes {
        let frame = regions[h.region].frame;
        let p_obj = frame.to_3d(h.center);
        let key_obj: [f64; 3] =
            std::array::from_fn(|i| h.axis[0] * frame.u[i] + h.axis[1] * frame.v[i]);
        let key = apply_linear(&target.transform, key_obj);
        let base_below = apply(&target.transform, p_obj);
        let base_above = match frame_above {
            Some(fa) if frame_below.is_some() => {
                apply(&target.transform, fa.to_3d(fa.to_2d(p_obj)))
            }
            _ => base_below,
        };
        if dovetail {
            let region = &regions[h.region];
            let (lo, hi) = region.outer.iter().fold(
                ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]),
                |(lo, hi), p| {
                    (
                        [lo[0].min(p[0]), lo[1].min(p[1])],
                        [hi[0].max(p[0]), hi[1].max(p[1])],
                    )
                },
            );
            let span = ((hi[0] - lo[0]).powi(2) + (hi[1] - lo[1]).powi(2)).sqrt();
            let wide = d + 2.0 * depth_below * dovetail::ANGLE_DEGREES.to_radians().tan();
            let tail_half = dovetail::tail_half_length(
                region,
                h.center,
                h.axis,
                (wide / 2.0 + min_wall) / scale,
                span,
            ) * scale;
            if tail_half <= 0.0 || depth_below <= allowance {
                return Err(failed(
                    "section_too_thin",
                    format!(
                        "Section {} is too small for a {d:.2} dovetail {depth_below:.2} deep. Use a \
                         smaller hole diameter or depth, or Alignment: Dowels.",
                        h.region + 1
                    ),
                ));
            }
            let (groove, tail) = dovetail::groove_and_tail(
                base_below,
                base_above,
                world_n,
                key,
                d,
                depth_below,
                clearance,
                allowance,
                span * scale,
                tail_half,
            );
            tools_below.push(groove);
            pegs_above.push(tail);
            continue;
        }
        let hole = pegs::profile(h.shape, d, chord_tol);
        let hole_tool = pegs::prism_at(base_below, world_n, key, &hole, -depth_below, over);
        if pegs == "attached" {
            let peg = pegs::dowel_profile(h.shape, d, clearance, chord_tol);
            let peg_mesh = pegs::prism_at(
                base_above,
                world_n,
                key,
                &peg,
                -attached_len,
                d.min(depth_above),
            );
            if fit_pair.is_none() {
                fit_pair = Some((hole_tool.clone(), peg_mesh.clone()));
            }
            pegs_above.push(peg_mesh);
        } else {
            let top_hole = pegs::prism_at(base_above, world_n, key, &hole, -over, depth_above);
            if fit_pair.is_none() {
                // Inset from the hole's own z-range (`-depth_below..over`)
                // so only the cylindrical wall is compared: a probe
                // flush with the hole's mouth and floor would touch
                // there regardless of the radial clearance, which isn't
                // what this check means to measure (a real dowel sits
                // `length_allowance` short of the hole's floor, D12).
                let margin = (depth_below * 0.1).min(allowance.max(1e-6));
                let dowel_profile = pegs::dowel_profile(h.shape, d, clearance, chord_tol);
                let dowel_mesh = pegs::prism_at(
                    base_below,
                    world_n,
                    key,
                    &dowel_profile,
                    -depth_below + margin,
                    over - margin,
                );
                fit_pair = Some((hole_tool.clone(), dowel_mesh));
            }
            tools_above.push(top_hole);
        }
        tools_below.push(hole_tool);
    }
    // The exact fit check (owner requirements 2026-11-11): measure the
    // real gap between the hole and its dowel/peg with the BVH minimum
    // distance, before the boolean consumes the tool meshes, and report
    // it in plain language against the Fit class and Machine profile
    // numbers recorded above. Never assumed: this always measures the
    // meshes as built, not the parameters that were meant to produce
    // them.
    let fit_report = fit_pair
        .map(|(hole_tool, peg)| -> Result<serde_json::Value, OpError> {
            let bvh = Bvh::build(&hole_tool, ctx).map_err(|_| OpError::Cancelled)?;
            // peg_clearance is diametral (it shrinks the dowel's whole
            // diameter); minimum_distance measures the radial gap, so
            // the fit class and machine numbers compared against it are
            // halved here, not at the source (the recorded values stay
            // the diametral ones the rest of this operation uses).
            let fit_class = FitClass {
                name: "Recorded".into(),
                clearance: fit_clearance / 2.0,
            };
            let machine = MachineProfile {
                name: "Recorded".into(),
                calibrated: machine_tolerance != 0.0,
                outer_xy: 0.0,
                hole_undersize: machine_tolerance / 2.0,
                z: 0.0,
            };
            let check = check_fit(
                "Peg to hole",
                MatingSurfaces {
                    a: &hole_tool,
                    a_bvh: &bvh,
                    b: &peg,
                    b_to_a: &DMat4::IDENTITY,
                },
                FitExpectation {
                    fit_class: &fit_class,
                    machine: &machine,
                    compensation: Compensation::Hole,
                },
                ctx,
            )
            .map_err(|_| OpError::Cancelled)?;
            Ok(check.map_or(serde_json::Value::Null, |c| {
                serde_json::json!({
                    "pair": c.pair,
                    "measured_gap": c.measured_gap,
                    "expected_gap": c.expected_gap,
                    "may_fuse": c.may_fuse,
                    "message": c.message(),
                })
            }))
        })
        .transpose()?
        .unwrap_or(serde_json::Value::Null);
    let into_object = |tools: &[Mesh]| -> Result<Mesh, OpError> {
        mf_boolean::operand_in_frame(
            &pegs::merged(tools),
            &crate::affine::IDENTITY,
            &target.transform,
        )
        .ok_or_else(|| {
            failed(
                "singular_transform",
                "The object's transform can't be inverted.",
            )
        })
    };
    let edit_part = |part: &mut Option<HalfCut>,
                     tools: &[Mesh],
                     op: mf_boolean::BoolOp|
     -> Result<(), OpError> {
        let Some(part) = part.as_mut() else {
            return Ok(());
        };
        if tools.is_empty() {
            return Ok(());
        }
        let tool = into_object(tools)?;
        let (mesh, prov) = pegs::apply_tool(&part.mesh, &tool, op, ctx)?;
        part.provenance = pegs::compose(&part.provenance, &prov);
        part.cap_faces.retain(|f| mesh.is_face_live(*f));
        part.mesh = mesh;
        Ok(())
    };
    edit_part(below, &tools_below, mf_boolean::BoolOp::Subtract)?;
    edit_part(above, &tools_above, mf_boolean::BoolOp::Subtract)?;
    edit_part(above, &pegs_above, mf_boolean::BoolOp::Union)?;
    // Dowels: upright, laid out on the plate.
    let mut dowels = Vec::new();
    let mut report_dowels = Vec::new();
    let mut off_plate = Vec::new();
    if pegs == "dowels" && !holes.is_empty() {
        let mut meshes = Vec::new();
        let mut prints = Vec::new();
        for h in &holes {
            let profile = pegs::dowel_profile(h.shape, d, clearance, chord_tol);
            let mesh = mf_boolean::primitives::chamfered_dowel(
                &mf_boolean::primitives::Axis::z_up([0.0, 0.0, 0.0]),
                &profile,
                dowel_len,
                1.5 * clearance,
            );
            prints.push(footprint(&mesh, &crate::affine::IDENTITY).unwrap_or([0.0; 4]));
            meshes.push(mesh);
        }
        // Clear of the split object and of everything else on the plate.
        let occupied: Vec<Footprint> = footprint(target.mesh, &target.transform)
            .into_iter()
            .chain(scene.iter().map(|o| o.footprint()))
            .collect();
        let layout = layout_on_plate(&occupied, &prints, &plate, spacing);
        off_plate = layout.off_plate.clone();
        for (mesh, t) in meshes.into_iter().zip(layout.transforms) {
            report_dowels.push(serde_json::json!({ "diameter": dowel_d, "length": dowel_len, "at": [t[9], t[10]] }));
            dowels.push((mesh, t));
        }
    }
    let message = (!off_plate.is_empty()).then(|| {
        format!(
            "{} dowel{} didn't fit on the plate; {} placed beside it",
            off_plate.len(),
            if off_plate.len() == 1 { "" } else { "s" },
            if off_plate.len() == 1 {
                "it's"
            } else {
                "they're"
            }
        )
    });
    Ok((
        dowels,
        serde_json::json!({
            "kind": pegs,
            "dovetail_angle_degrees": dovetail.then_some(dovetail::ANGLE_DEGREES),
            "holes": holes.len(),
            "hole_diameter": d,
            "dowel_diameter": dowel_d,
            "dowel_length": if pegs == "dowels" { dowel_len } else { attached_len },
            "dowels": report_dowels,
            "off_plate": off_plate,
            "off_plate_message": message,
            "skipped_regions": skipped,
            "fit_clearance": fit_clearance,
            "machine_tolerance": machine_tolerance,
            "fit_check": fit_report,
        }),
    ))
}
