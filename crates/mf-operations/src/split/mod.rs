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
//! - **Several targets** (the target is `many`): each is cut as above, as
//!   its own operand, in one command; one the plane misses is listed in
//!   the report's `missed` (refused only when it misses them all).
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
//!   `dovetail` (Task 14, see [`dovetail`]) cuts a trapezoidal groove and
//!   tail pair at each hole instead of a round dowel/peg, sized the same
//!   way from the fit profile.

use glam::DMat4;
use mf_boolean::{CutError, HalfCut};
use mf_geometry::{Bvh, Mesh, Provenance, Sources};
use mf_parts::{Compensation, FitClass, FitExpectation, MachineProfile, MatingSurfaces, check_fit};
use mf_spec::Params;

use crate::layout::{Footprint, Plate, layout_on_plate};
use mf_geometry::affine::apply;

pub mod dovetail;
pub mod joints;
pub mod pegs;
pub mod plan;
pub mod region;
pub mod volume;
use crate::{
    DocContext, DocOutput, ObjectChange, OpError, OpOutput, OperandInput, Operation, RunContext,
    Target, needs_document,
};
use pegs::PegShape;
use plan::{CutPlan, PegPlan, SplitPlan, TargetPlan};

/// The stages a split reports, in order.
pub const SPLIT_STAGES: &[&str] = &[
    mf_boolean::CHECKING,
    mf_boolean::FINDING_THE_CUT,
    mf_boolean::CUTTING,
    mf_boolean::CAPPING,
];

/// The two sides a split's plane divides the model into, named by
/// orientation rather than by the raw "above"/"below" the normal points
/// to, which means nothing for a vertical or tilted cut (owner
/// 2026-11-12). `SideA` is always the side the plane's normal points to
/// (`keep: above`'s side); `SideB` is the other (`keep: below`'s side).
/// `keep`'s values and their meaning don't change — only the words a
/// part's name and the Keep buttons show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SideNames {
    /// Normal within [`AXIS_ALIGN_DEGREES`] of +-Z: "top" (the side it
    /// points to, +Z-ish) and "bottom".
    TopBottom,
    /// A vertical plane (normal within [`AXIS_ALIGN_DEGREES`] of the XY
    /// plane) leaning more on X than Y: "right" (+X-ish, the front
    /// view's right, looking from -Y toward +Y) and "left".
    LeftRight,
    /// A vertical plane leaning more on Y than X: "back" (+Y-ish) and
    /// "front".
    FrontBack,
    /// Any other tilt: "side a" (the normal's side) / "side b".
    SideAB,
}

/// Within this many degrees of an axis (or the horizontal plane), a cut
/// counts as aligned with it rather than tilted (owner 2026-11-12: 20
/// degrees either side of +-Z is "horizontal", not "a bit tilted").
pub const AXIS_ALIGN_DEGREES: f64 = 20.0;

impl SideNames {
    /// [`SideNames`] for a plane with this world `normal` (any length
    /// but zero): horizontal (its angle from +-Z is within
    /// [`AXIS_ALIGN_DEGREES`]) is `TopBottom`; vertical (its angle from
    /// the horizontal XY plane is within that same tolerance) is
    /// `LeftRight` or `FrontBack`, by whichever of X or Y it leans on
    /// more; anything else (a genuine tilt, between the two) is
    /// `SideAB`.
    pub fn for_normal(normal: [f64; 3]) -> SideNames {
        let n = glam::DVec3::from(normal).normalize_or(glam::DVec3::Z);
        let angle_from_z = n.z.abs().clamp(0.0, 1.0).acos().to_degrees();
        if angle_from_z <= AXIS_ALIGN_DEGREES {
            SideNames::TopBottom
        } else if angle_from_z >= 90.0 - AXIS_ALIGN_DEGREES {
            if n.x.abs() >= n.y.abs() {
                SideNames::LeftRight
            } else {
                SideNames::FrontBack
            }
        } else {
            SideNames::SideAB
        }
    }

    /// The lowercase suffix a kept part's name takes ("scan (top)",
    /// "scan (left)"...), for side a (`keep: above`) or side b
    /// (`keep: below`).
    pub fn suffix(&self, side_a: bool) -> &'static str {
        match (self, side_a) {
            (SideNames::TopBottom, true) => "top",
            (SideNames::TopBottom, false) => "bottom",
            (SideNames::LeftRight, true) => "right",
            (SideNames::LeftRight, false) => "left",
            (SideNames::FrontBack, true) => "back",
            (SideNames::FrontBack, false) => "front",
            (SideNames::SideAB, true) => "side a",
            (SideNames::SideAB, false) => "side b",
        }
    }
}

/// The names a split gives its parts by the plane's `normal` (world
/// space): `(side a's name, side b's name)`, i.e. `(keep: above`'s name,
/// `keep: below`'s name)`.
pub fn split_names(name: &str, normal: [f64; 3]) -> (String, String) {
    let sides = SideNames::for_normal(normal);
    (
        format!("{name} ({})", sides.suffix(true)),
        format!("{name} ({})", sides.suffix(false)),
    )
}

pub struct SplitPlane;

/// A malformed invocation (an unreadable mesh, ADR-0010): never one of
/// `mesh.split_plane`'s declared refusals, which [`refusal`] builds
/// through the one registry helper instead. On the `OpError::Failed`
/// allow-list, `docs/failed-refusals.allow`.
fn failed(code: &'static str, message: impl Into<String>) -> OpError {
    OpError::Failed {
        code,
        message: message.into(),
    }
}

/// Builds the [`OpError::Refused`] for `code` (ADR-0010, through the
/// one registry helper), with `message` as the `detail` argument
/// `specs/operations/mesh.split_plane.yaml`'s message fills in.
fn refuse(code: &'static str, message: impl Into<String>) -> OpError {
    crate::refusal::refuse(
        code,
        std::collections::BTreeMap::from([(
            "detail",
            crate::refusal::RefusalArg::Text(message.into()),
        )]),
    )
}

/// As [`refuse`], naming a point ([`mf_units::Length`] × 3, never a bare
/// `{:.3}`) as well as a `detail`.
fn refuse_at(code: &'static str, at: [f64; 3], message: impl Into<String>) -> OpError {
    use crate::refusal::RefusalArg;
    crate::refusal::refuse(
        code,
        std::collections::BTreeMap::from([
            ("x", RefusalArg::Text(mf_spec::format_length(at[0]))),
            ("y", RefusalArg::Text(mf_spec::format_length(at[1]))),
            ("z", RefusalArg::Text(mf_spec::format_length(at[2]))),
            ("detail", RefusalArg::Text(message.into())),
        ]),
    )
}

/// The refusals, with what to do next (the copy table's plain words).
/// `transform` is the target's (object → world), so a self-intersection's
/// `at` — found in object space, where [`mf_boolean::cut_half`] works —
/// is reported in the same world/document frame as the Split panel's own
/// Point fields, not the object's local one (owner 2026-11-12).
fn refusal(transform: &[f64; 12], e: CutError) -> OpError {
    match e {
        CutError::Cancelled => OpError::Cancelled,
        CutError::NotClosed { boundary_edges, .. } => refuse(
            "operand_not_closed",
            format!(
                "This model isn't closed ({boundary_edges} open edges), so a cut has no inside \
                 to cap. Close holes first."
            ),
        ),
        CutError::MissesObject { .. } => refuse(
            "plane_misses_object",
            "The cut plane doesn't pass through the object (or the gap is wider than it). Move \
             the plane so it crosses the model.",
        ),
        CutError::SectionSelfIntersects { at } => {
            let at = apply(transform, at);
            refuse_at(
                // A cut's own code, not `mesh.boolean`'s
                // "section_self_intersects". Owner 2026-11-13: a cut through
                // valid closed manifold geometry is never refused, wherever
                // its vertices sit relative to the plane — on it, an ulp off
                // it, whole rings of them — and loops that only touch are
                // split apart, not refused (`mf_boolean::plane_cut`). What's
                // left is a section whose loops genuinely cross, which only
                // a model whose surface crosses itself makes: Make solid is
                // the fix, and the next action offered.
                "split_section_self_intersects",
                at,
                "The model's surface crosses itself where the cut passes. Run Make solid on it \
                 first.",
            )
        }
        // The plane lies along the surface (through a mesh edge or face)
        // closer than the coordinates can separate, and rounding left the
        // section impossible to close as a clean solid: refused by name,
        // never written as a part that isn't closed and manifold. The
        // model itself is fine, so Make solid isn't offered.
        CutError::SectionDegenerate { at } => {
            let at = apply(transform, at);
            refuse_at(
                "split_section_degenerate",
                at,
                "The cut runs exactly along the model's surface here, closer than its \
                 coordinates can separate; move the plane slightly.",
            )
        }
        CutError::InvalidPlane => refuse(
            "invalid_plane",
            "The cut plane's direction is zero; give it a direction.",
        ),
        // Phase 1b, the bounded cut plane: a region's section straddles
        // the bound's edge, ambiguous which way it should go.
        CutError::RegionCrossesBound { .. } => refuse(
            "split_region_crosses_bound",
            "A cut region crosses the cut area's edge. Make the cut area larger or move it.",
        ),
        CutError::Geometry(e) => refuse("invalid_mesh", e.to_string()),
    }
}

/// A part's provenance as a created object's: its created faces name
/// their sources as faces of operand `k` (the target it was cut from).
fn as_created(mut p: Provenance, k: usize) -> Provenance {
    for (_, s) in &mut p.created_faces {
        let faces = std::mem::take(&mut s.faces);
        *s = Sources {
            operands: faces.into_iter().map(|f| (k as u32, f)).collect(),
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
        4
    }

    fn upgrade_params(&self, recorded: u32, params: &mut Params) -> bool {
        match recorded {
            // Version 1's round alignment holes were always round.
            1 => {
                params.0.insert(
                    "hole_profile".to_owned(),
                    mf_spec::ParamValue::Text("round".into()),
                );
                self.upgrade_params(2, params)
            }
            // Version 2 (and 1, falling through) always capped.
            2 => {
                params
                    .0
                    .insert("cap".to_owned(), mf_spec::ParamValue::Bool(true));
                self.upgrade_params(3, params)
            }
            // Version 3 (and older, falling through) always cut every
            // region, with no bound (Phase 1b: regions, bound).
            3 => {
                params.0.insert(
                    "regions".to_owned(),
                    mf_spec::ParamValue::Faces(mf_spec::FaceList::default()),
                );
                params
                    .0
                    .insert("bound".to_owned(), mf_spec::ParamValue::Bool(false));
                params.0.insert(
                    "bound_shape".to_owned(),
                    mf_spec::ParamValue::Text("disc".into()),
                );
                params
                    .0
                    .insert("bound_width".to_owned(), mf_spec::ParamValue::Float(100.0));
                params
                    .0
                    .insert("bound_height".to_owned(), mf_spec::ParamValue::Float(100.0));
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
        _doc: &DocContext,
        ctx: &RunContext,
    ) -> Result<DocOutput, OpError> {
        let plan = plan::plan(inputs, params, ctx)?;
        execute(plan, inputs, scene, params, ctx)
    }
}

/// With several targets, a refusal or failure names the target it's
/// about; with one, it's returned as is.
fn name_target(targets: usize, target: &OperandInput, e: OpError) -> OpError {
    match e {
        OpError::Refused { code, mut args } if targets > 1 => {
            args.insert(
                "object",
                crate::refusal::RefusalArg::Text(target.name.to_owned()),
            );
            crate::refusal::refuse(code, args)
        }
        OpError::Failed { code, message } if targets > 1 => OpError::Failed {
            code,
            message: format!("'{}': {message}", target.name),
        },
        e => e,
    }
}

/// Carries out `plan` (from [`plan::plan`] on these same `inputs` and
/// `params`): the boolean edits for the alignment features, the dowels'
/// layout, and the changes and report. Nothing the plan decided is
/// decided again here (ADR-0008).
pub fn execute(
    plan: SplitPlan,
    inputs: &[OperandInput],
    scene: &[crate::SceneObject],
    params: &Params,
    ctx: &RunContext,
) -> Result<DocOutput, OpError> {
    // Each target is cut on its own; dowels made for one are laid out
    // clear of those made for the ones before it.
    let mut scene = scene.to_vec();
    let mut changes = Vec::new();
    let mut cut = Vec::new();
    let mut missed = Vec::new();
    // Each cut's joint (the Split workflow's step 3), as planned.
    let mut joints = Vec::new();
    for target_plan in plan.targets {
        let one_plan = match target_plan {
            TargetPlan::Missed { name, .. } => {
                missed.push(name);
                continue;
            }
            TargetPlan::Cut(one) => one,
        };
        let target = inputs.get(one_plan.operand).ok_or_else(|| {
            failed(
                "plan_mismatch",
                "mesh.split_plane's plan names a target it wasn't given",
            )
        })?;
        if let Some(joint) = &one_plan.joint {
            joints.push(joint.report());
        }
        let one = execute_one(*one_plan, target, &scene, params, ctx)
            .map_err(|e| name_target(inputs.len(), target, e))?;
        for c in &one.changes {
            if let ObjectChange::Create {
                mesh, transform, ..
            } = c
            {
                scene.extend(crate::SceneObject::of(target.object, mesh, transform, ctx)?);
            }
        }
        changes.extend(one.changes);
        cut.push(one.report);
    }
    // The first cut object's report at the top level (the one-target
    // shape), then every cut object's and the missed ones. The plan
    // always holds at least one cut (missing every target is refused).
    let Some(first) = cut.first() else {
        return Err(failed(
            "plan_mismatch",
            "mesh.split_plane's plan cuts no target",
        ));
    };
    let mut report = first.clone();
    let parts: Vec<serde_json::Value> = cut
        .iter()
        // allow-swallow: each `r` is execute_one's own report, which always sets "parts" to an array.
        .flat_map(|r| r["parts"].as_array().cloned().unwrap_or_default())
        .collect();
    report["parts"] = serde_json::Value::Array(parts);
    report["objects"] = serde_json::Value::Array(cut);
    report["missed"] = serde_json::json!(missed);
    report["joints"] = serde_json::Value::Array(joints);
    Ok(DocOutput { changes, report })
}

/// One target's split: its changes (operand `k`) and report.
struct SplitOne {
    changes: Vec<ObjectChange>,
    report: serde_json::Value,
}

fn execute_one(
    one: CutPlan,
    target: &OperandInput,
    scene: &[crate::SceneObject],
    params: &Params,
    ctx: &RunContext,
) -> Result<SplitOne, OpError> {
    let k = one.operand;
    let regions = one.regions().len();
    let plane_point_object = one.plane_above.point;
    let object_normal = one.plane_above.normal;
    let mut above = one.above;
    let mut below = one.below;
    let mut created_dowels = Vec::new();
    let mut peg_report = serde_json::Value::Null;
    if let Some(peg_plan) = &one.pegs {
        let out = alignment(
            params,
            peg_plan,
            target,
            scene,
            one.world_normal,
            &mut above,
            &mut below,
            ctx,
        )?;
        created_dowels = out.0;
        peg_report = out.1;
    }
    let (top_name, bottom_name) = one.names;
    let mut changes = Vec::new();
    let mut parts = Vec::new();
    match (above, below) {
        (Some(above), Some(below)) => {
            parts.push(part_report(&top_name, "target", &above));
            parts.push(part_report(&bottom_name, "created", &below));
            changes.push(ObjectChange::Geometry {
                operand: k,
                mesh: above.mesh,
                provenance: above.provenance,
            });
            changes.push(ObjectChange::Rename {
                operand: k,
                name: top_name,
            });
            changes.push(ObjectChange::Create {
                name: bottom_name,
                mesh: below.mesh,
                transform: target.transform,
                provenance: as_created(below.provenance, k),
            });
        }
        (Some(part), None) => {
            // Above only: by request (`keep: above`), or `keep: both`
            // with Below genuinely empty (the plane lies exactly on the
            // model's lower surface there).
            parts.push(part_report(&top_name, "target", &part));
            changes.push(ObjectChange::Geometry {
                operand: k,
                mesh: part.mesh,
                provenance: part.provenance,
            });
            changes.push(ObjectChange::Rename {
                operand: k,
                name: top_name,
            });
        }
        (None, Some(part)) => {
            parts.push(part_report(&bottom_name, "target", &part));
            changes.push(ObjectChange::Geometry {
                operand: k,
                mesh: part.mesh,
                provenance: part.provenance,
            });
            changes.push(ObjectChange::Rename {
                operand: k,
                name: bottom_name,
            });
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
    Ok(SplitOne {
        changes,
        report: serde_json::json!({
            "parts": parts,
            "section_regions": regions,
            "plane_point_object": plane_point_object,
            "plane_normal_object": object_normal,
            "pegs": peg_report,
        }),
    })
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

/// A round alignment hole's outline with the hole profile asked for
/// (fix item 9, D24), in `pegs::prism_at`'s own frame (x along `key`
/// projected onto the cut, y = normal × x, z the normal): under `auto` a
/// teardrop if the hole's axis (the cut's normal) is within 45 degrees
/// of horizontal, its point facing world up; otherwise the choice. Also
/// the profile it used, for the report.
fn round_alignment_hole(
    choice: &str,
    r: f64,
    n: [f64; 3],
    key: [f64; 3],
    chord_tol: f64,
) -> (Vec<[f64; 2]>, &'static str) {
    use glam::{DMat3, DVec3};
    use mf_parts::connectors::hole_profile::{HoleProfile, axis_tilt, hole_outline, local_up};
    let z = DVec3::from(n).normalize_or(DVec3::Z);
    let k = DVec3::from(key);
    let mut x = k - z * k.dot(z);
    if x.length() < 1e-12 {
        x = z.any_orthonormal_vector();
    }
    let x = x.normalize();
    let y = z.cross(x);
    let frame = DMat3::from_cols(x, y, z);
    let profile = match choice {
        "teardrop" => HoleProfile::Teardrop,
        "flat_top" => HoleProfile::FlatTop,
        "round" => HoleProfile::Round,
        _ => HoleProfile::auto(axis_tilt(frame, DVec3::Z)),
    };
    let up = local_up(frame, DVec3::X, DVec3::Y, DVec3::Z).unwrap_or([0.0, 1.0]);
    let name = match profile {
        HoleProfile::Round => "round",
        HoleProfile::Teardrop => "teardrop",
        HoleProfile::FlatTop => "flat_top",
    };
    (hole_outline(profile, r, up, 45.0, chord_tol), name)
}

/// Holes, attached pegs and dowels (D12), where `peg_plan` placed them.
/// Changes the parts in place (their provenance stays relative to the
/// target's input) and returns the dowels to create, each with its
/// placement, and a report.
#[allow(clippy::too_many_arguments)]
fn alignment(
    params: &Params,
    peg_plan: &PegPlan,
    target: &OperandInput,
    scene: &[crate::SceneObject],
    world_n: [f64; 3],
    above: &mut Option<HalfCut>,
    below: &mut Option<HalfCut>,
    ctx: &RunContext,
) -> Result<(Vec<Placed>, serde_json::Value), OpError> {
    let pegs = peg_plan.kind.as_str();
    let holes = &peg_plan.holes;
    let hole_profile = params.get_choice("hole_profile")?;
    let mut profile_used: Option<&'static str> = None;
    let d = params.get_float("hole_diameter")?;
    let depth_above = params.get_float("hole_depth_above")?;
    let depth_below = params.get_float("hole_depth_below")?;
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
    let plate = Plate {
        size: [
            params.get_float("plate_width")?,
            params.get_float("plate_depth")?,
        ],
    };
    let spacing = params.get_float("plate_spacing")?;
    let dowel_d = peg_plan.dowel_diameter;
    let dowel_len = peg_plan.dowel_length;
    let attached_len = peg_plan.attached_length;
    let chord_tol = if clearance > 0.0 {
        clearance / 10.0
    } else {
        d / 1000.0
    };
    let dovetail = pegs == "dovetail";
    let over = d + params.get_float("cut_gap")?;
    let mut tools_below = Vec::new();
    let mut tools_above = Vec::new();
    let mut pegs_above = Vec::new();
    // The first hole's mating pair, in world space, for the exact fit
    // check below: (hole tool mesh, dowel-or-peg mesh at the hole's own
    // clearance). Captured before the boolean consumes the tool meshes.
    let mut fit_pair: Option<(Mesh, Mesh)> = None;
    for planned in holes {
        let h = &planned.placement;
        let (key, base_below, base_above) = (planned.key, planned.base_below, planned.base_above);
        if let Some(fit) = planned.dovetail {
            let (groove, tail) = dovetail::groove_and_tail(
                base_below,
                base_above,
                world_n,
                key,
                d,
                depth_below,
                clearance,
                allowance,
                fit.groove_half,
                fit.tail_half,
            );
            tools_below.push(groove);
            pegs_above.push(tail);
            continue;
        }
        let hole = if h.shape == PegShape::Round {
            let (outline, used) =
                round_alignment_hole(hole_profile, d / 2.0, world_n, key, chord_tol);
            profile_used = Some(used);
            outline
        } else {
            pegs::profile(h.shape, d, chord_tol)
        };
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
                print_volume_mm: MachineProfile::uncalibrated().print_volume_mm,
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
            &mf_geometry::affine::IDENTITY,
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
        for planned in holes {
            let h = &planned.placement;
            let profile = pegs::dowel_profile(h.shape, d, clearance, chord_tol);
            let mesh = mf_boolean::primitives::chamfered_dowel(
                &mf_boolean::primitives::Axis::z_up([0.0, 0.0, 0.0]),
                &profile,
                dowel_len,
                1.5 * clearance,
            );
            prints.push(footprint(&mesh, &mf_geometry::affine::IDENTITY).unwrap_or([0.0; 4]));
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
            "hole_profile": profile_used,
            "dowel_diameter": dowel_d,
            "dowel_length": if pegs == "dowels" { dowel_len } else { attached_len },
            "dowels": report_dowels,
            "off_plate": off_plate,
            "off_plate_message": message,
            "skipped_regions": peg_plan.skipped_regions,
            "fit_clearance": fit_clearance,
            "machine_tolerance": machine_tolerance,
            "fit_check": fit_report,
        }),
    ))
}
