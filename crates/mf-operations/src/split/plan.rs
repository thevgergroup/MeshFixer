//! The Split plan (C22, ADR-0008 part 1): everything `mesh.split_plane`
//! decides before it edits anything, computed once by [`plan`] and
//! consumed whole by [`super::execute`]. A preview renders this plan and
//! Apply executes the same one, so the two can't diverge: Apply never
//! recomputes the planes, the regions, the hole placements, the joints or
//! which targets the plane misses.
//!
//! - **Planes**: the world plane (shifted by `cut_gap / 2` each way) in
//!   each target's object space, the normal by
//!   [`mf_geometry::affine::normal_to_object`] (`M · n`, right under a
//!   non-uniform scale).
//! - **Regions**: the cut itself, both kept sides ([`HalfCut`]), whose
//!   `section_regions` are what the caps cover.
//! - **Pegs**: where each hole goes (world base points on both cut
//!   planes, the key direction) and the dowel's size; the declared
//!   refusals (`dowel_too_short`, `dowel_too_thin`, `section_too_thin`)
//!   are decided here.
//! - **Joints**: each cut's joint, with the created-object index Apply's
//!   bottom part lands at.
//! - **Plane-miss**: with several targets, one the plane misses is
//!   [`TargetPlan::Missed`]; missing them all is refused here.
//!
//! What stays in execute: the boolean edits for the holes, pegs and
//! dovetails, the exact fit measurement, and laying the dowels out on
//! the plate (it needs the footprints of the parts already made).

use mf_boolean::{CutError, HalfCut, Plane, SectionRegion, Side};
use mf_geometry::affine::{apply, apply_linear, determinant, inverse, normal_to_object};
use mf_spec::Params;

use super::pegs::{self, HolePlacement, PegError, PegShape};
use super::{dovetail, failed, refusal, refuse, split_names};
use crate::{OpError, OperandInput, RunContext, Target, unsupported_target};

/// What `mesh.split_plane` will do to every target, in operand order.
#[derive(Debug, Clone)]
pub struct SplitPlan {
    pub targets: Vec<TargetPlan>,
}

impl SplitPlan {
    /// The names of the targets the plane misses (only ever non-empty
    /// with several targets: one target missed is refused instead).
    pub fn missed(&self) -> Vec<&str> {
        self.targets
            .iter()
            .filter_map(|t| match t {
                TargetPlan::Missed { name, .. } => Some(name.as_str()),
                TargetPlan::Cut(_) => None,
            })
            .collect()
    }

    /// Every cut's joint, in order (the report's `joints`).
    pub fn joints(&self) -> Vec<&JointPlan> {
        self.targets
            .iter()
            .filter_map(|t| match t {
                TargetPlan::Cut(c) => c.joint.as_ref(),
                TargetPlan::Missed { .. } => None,
            })
            .collect()
    }
}

/// One target's plan.
#[derive(Debug, Clone)]
pub enum TargetPlan {
    /// The plane crosses it: cut it like this.
    Cut(Box<CutPlan>),
    /// Several targets were given and the plane misses this one: it's
    /// left alone and listed in the report's `missed`.
    Missed { operand: usize, name: String },
}

/// How one target is cut.
#[derive(Debug, Clone)]
pub struct CutPlan {
    /// Its index among the operation's inputs.
    pub operand: usize,
    /// The plane `cut_gap / 2` along the normal, in object space: the top
    /// part (side a) is cut here.
    pub plane_above: Plane,
    /// The plane `cut_gap / 2` against the normal, in object space: the
    /// bottom part (side b) is cut here.
    pub plane_below: Plane,
    /// The plane's world normal, unit length.
    pub world_normal: [f64; 3],
    /// Side a (the normal's side), unless `keep: below` or it is
    /// genuinely empty (the plane grazes the model there).
    pub above: Option<HalfCut>,
    /// Side b, unless `keep: above` or it is genuinely empty.
    pub below: Option<HalfCut>,
    /// Side a's name, then side b's ("scan (top)", "scan (bottom)"...).
    pub names: (String, String),
    /// The alignment features, unless `pegs: none`.
    pub pegs: Option<PegPlan>,
    /// The joint this cut makes (the Split workflow's step 3), if it
    /// makes one.
    pub joint: Option<JointPlan>,
}

impl CutPlan {
    /// The section regions the holes are placed on: the bottom part's,
    /// or the top part's if only it is kept. Empty when uncapped.
    pub fn regions(&self) -> &[SectionRegion] {
        self.below
            .as_ref()
            .or(self.above.as_ref())
            .map_or(&[], |p| p.section_regions.as_slice())
    }

    /// How many objects Apply creates for this target: the bottom part
    /// when both are kept, then one per dowel.
    pub fn created_count(&self) -> usize {
        let part = usize::from(self.above.is_some() && self.below.is_some());
        let dowels = self
            .pegs
            .as_ref()
            .filter(|p| p.kind == "dowels")
            .map_or(0, |p| p.holes.len());
        part + dowels
    }
}

/// Where the alignment holes go and what fills them.
#[derive(Debug, Clone)]
pub struct PegPlan {
    /// `dowels`, `attached` or `dovetail`.
    pub kind: String,
    pub holes: Vec<PlannedHole>,
    /// The regions skipped as slivers.
    pub skipped_regions: Vec<usize>,
    /// Object units per world unit (the uniform-scale approximation the
    /// hole sizes are compared with the section in).
    pub scale: f64,
    pub dowel_diameter: f64,
    pub dowel_length: f64,
    /// An attached peg's length (`hole_depth_below − length_allowance`).
    pub attached_length: f64,
}

/// One hole, placed.
#[derive(Debug, Clone)]
pub struct PlannedHole {
    /// Its region and 2D placement in that region's (object-space) frame.
    pub placement: HolePlacement,
    /// Its centre on the bottom part's cut plane, world space.
    pub base_below: [f64; 3],
    /// Its centre on the top part's cut plane, world space.
    pub base_above: [f64; 3],
    /// The direction its key faces, world space (not normalised: the
    /// object frame's key carried by the transform's linear part).
    pub key: [f64; 3],
    /// A dovetail's lengths, world units (`pegs: dovetail` only).
    pub dovetail: Option<DovetailPlan>,
}

/// A dovetail groove's and tail's lengths along the key.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DovetailPlan {
    /// The groove's half length (it runs right through the part).
    pub groove_half: f64,
    /// The tail's half length (a wall short of the section's outline).
    pub tail_half: f64,
}

/// A cut's joint: the bottom part (created) and the top part (the
/// target), across the plane, normal from the bottom into the top.
#[derive(Debug, Clone, PartialEq)]
pub struct JointPlan {
    /// Apply's created-object index of the bottom part (counting every
    /// object the cuts before it create).
    pub created: usize,
    /// The target's operand index (the top part keeps its id).
    pub operand: usize,
    /// The world plane point and unit normal, and the gap.
    pub point: [f64; 3],
    pub normal: [f64; 3],
    pub gap: f64,
}

impl JointPlan {
    /// The report's form.
    pub fn report(&self) -> serde_json::Value {
        serde_json::json!({
            "a": { "created": self.created },
            "b": { "operand": self.operand },
            "point": self.point,
            "normal": self.normal,
            "gap": self.gap,
        })
    }
}

/// `mesh.split_plane`'s plan for `inputs` (every target, as given). A
/// refusal here is Apply's refusal.
pub fn plan(
    inputs: &[OperandInput],
    params: &Params,
    ctx: &RunContext,
) -> Result<SplitPlan, OpError> {
    if inputs.is_empty() {
        return Err(failed(
            "missing_target",
            "mesh.split_plane was run with no target",
        ));
    }
    let both = params.get_choice("keep")? == "both";
    let cap = params.get_bool("cap")?;
    let normal = params.get_vec3("plane_normal")?;
    let len = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
    let mut targets = Vec::with_capacity(inputs.len());
    let mut created = 0usize;
    for (k, target) in inputs.iter().enumerate() {
        match plan_one(k, target, params, ctx) {
            Ok(mut one) => {
                // A joint needs a genuine split: `keep: both` can still
                // create nothing for this target when the plane lies
                // exactly on its surface and one side is empty (owner
                // 2026-11-12) — no second piece, so no cut face pair to
                // report as a joint. Uncapped (a vase or bowl), there's
                // no cap face pair to place connectors against either.
                let made = one.created_count();
                if both && len > 0.0 && cap && made > 0 {
                    one.joint = Some(JointPlan {
                        created,
                        operand: k,
                        point: params.get_vec3("plane_point")?,
                        normal: normal.map(|c| c / len),
                        gap: params.get_float("cut_gap")?,
                    });
                }
                created += made;
                targets.push(TargetPlan::Cut(Box::new(one)));
            }
            // With several targets, one the plane misses is listed, not
            // refused (unless the plane misses them all).
            Err(OpError::Refused {
                code: "plane_misses_object",
                ..
            }) if inputs.len() > 1 => targets.push(TargetPlan::Missed {
                operand: k,
                name: target.name.to_owned(),
            }),
            Err(e) => return Err(super::name_target(inputs.len(), target, e)),
        }
    }
    if targets
        .iter()
        .all(|t| matches!(t, TargetPlan::Missed { .. }))
    {
        return Err(refuse(
            "plane_misses_object",
            "The cut plane doesn't pass through any of the selected objects (or the gap is \
             wider than them). Move the plane so it crosses a model.",
        ));
    }
    Ok(SplitPlan { targets })
}

/// One target's [`CutPlan`] (its joint is filled in by [`plan`]).
fn plan_one(
    k: usize,
    target: &OperandInput,
    params: &Params,
    ctx: &RunContext,
) -> Result<CutPlan, OpError> {
    if target.target != Target::WholeObject {
        return Err(unsupported_target("mesh.split_plane", &target.target));
    }
    let point = params.get_vec3("plane_point")?;
    let normal = params.get_vec3("plane_normal")?;
    let keep = params.get_choice("keep")?;
    let gap = params.get_float("cut_gap")?;
    let pegs = params.get_choice("pegs")?;
    let cap = params.get_bool("cap")?;
    // Cross-section picking (Phase 1b): an empty list cuts every region;
    // `mf_boolean::cut_half_with_regions` takes the same "empty/absent
    // means all" convention.
    let region_faces: Vec<mf_geometry::FaceId> = params
        .get_faces("regions")?
        .into_iter()
        .map(mf_geometry::FaceId)
        .collect();
    let regions_filter = (!region_faces.is_empty()).then_some(region_faces);
    // The bounded cut plane (Phase 1b): width/height are world mm; the
    // bound itself lives in the plane's own object-space frame, so they
    // need the same object-units conversion the hole sizes use.
    let scale = object_scale(&target.transform);
    let bound = params.get_bool("bound")?.then(|| {
        let width = params.get_float("bound_width").unwrap_or(100.0) / scale;
        let height = params.get_float("bound_height").unwrap_or(100.0) / scale;
        match params.get_choice("bound_shape").unwrap_or("disc") {
            "rect" => mf_boolean::Bound::Rect { width, height },
            _ => mf_boolean::Bound::Disc {
                radius: width / 2.0,
            },
        }
    });
    let len = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
    if len == 0.0 || !len.is_finite() {
        return Err(refusal(&target.transform, CutError::InvalidPlane));
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
    mf_boolean::check_closed(target.mesh, ctx).map_err(|e| refusal(&target.transform, e))?;
    // A side genuinely empty because the plane grazes the model's surface
    // exactly there, with nothing strictly past it (`CutError::
    // MissesObject { grazed: true }` on just one side), is not a refusal
    // with `keep: both`: it degrades to keeping only the other side, the
    // same as if `keep` had asked for it by name, rather than failing a
    // cut that a plane lying exactly on a face must never be refused for
    // (owner 2026-11-12). A plane that doesn't touch the model's surface
    // at all (`grazed: false`) is still a genuine miss; so is asking for
    // just one side and missing it entirely.
    let side_or_none = |keep_this: bool, plane: &Plane, side: Side| {
        keep_this
            .then(|| {
                mf_boolean::cut_half_with_regions(
                    target.mesh,
                    plane,
                    side,
                    cap,
                    regions_filter.as_deref(),
                    bound.as_ref(),
                    ctx,
                )
            })
            .transpose()
    };
    let above = match side_or_none(keep != "below", &plane_above, Side::Above) {
        Ok(above) => above,
        Err(CutError::MissesObject { grazed: true }) if keep == "both" => None,
        Err(e) => return Err(refusal(&target.transform, e)),
    };
    let below = match side_or_none(keep != "above", &plane_below, Side::Below) {
        Ok(below) => below,
        Err(CutError::MissesObject { grazed: true }) if keep == "both" => None,
        Err(e) => return Err(refusal(&target.transform, e)),
    };
    if above.is_none() && below.is_none() {
        return Err(refusal(
            &target.transform,
            CutError::MissesObject { grazed: false },
        ));
    }
    ctx.checkpoint()?;
    let mut cut = CutPlan {
        operand: k,
        plane_above,
        plane_below,
        world_normal: unit,
        above,
        below,
        names: split_names(target.name, unit),
        pegs: None,
        joint: None,
    };
    if pegs != "none" {
        cut.pegs = Some(plan_pegs(params, pegs, target, &cut, ctx)?);
    }
    Ok(cut)
}

/// Object units per world unit (uniform-scale approximation): sizes
/// compared with the section, or bounding it, are in object units.
pub(super) fn object_scale(transform: &[f64; 12]) -> f64 {
    let scale = determinant(transform).abs().cbrt();
    if scale > 0.0 && scale.is_finite() {
        scale
    } else {
        1.0
    }
}

/// Where the holes go (D12): sized and refused here, built by execute.
fn plan_pegs(
    params: &Params,
    pegs: &str,
    target: &OperandInput,
    cut: &CutPlan,
    ctx: &RunContext,
) -> Result<PegPlan, OpError> {
    let shape = match params.get_choice("peg_shape")? {
        "square" => PegShape::Square,
        "keyed" => PegShape::Keyed,
        _ => PegShape::Round,
    };
    let d = params.get_float("hole_diameter")?;
    let depth_above = params.get_float("hole_depth_above")?;
    let depth_below = params.get_float("hole_depth_below")?;
    let count = params.get_int("peg_count")?.max(0) as u32;
    // D20 migration: `peg_clearance` is what sizes the hole and dowel (a
    // frontend fills it from `fit_clearance + machine_tolerance`).
    let clearance = params.get_float("peg_clearance")?;
    let allowance = params.get_float("length_allowance")?;
    let min_wall = params.get_float("min_wall")?;
    let (dowel_d, dowel_len) = pegs::dowel_size(d, depth_above, depth_below, clearance, allowance);
    let attached_len = depth_below - allowance;
    if (pegs == "dowels" && dowel_len <= 0.0) || (pegs == "attached" && attached_len <= 0.0) {
        return Err(refuse(
            "dowel_too_short",
            "The holes are too shallow for a dowel after the length allowance. Make the holes \
             deeper.",
        ));
    }
    if dowel_d <= 0.0 || d <= 0.0 {
        return Err(refuse(
            "dowel_too_thin",
            "The hole is no wider than the clearance, so the dowel would have no width. Make \
             the hole wider.",
        ));
    }
    let scale = object_scale(&target.transform);
    let regions = cut.regions();
    let dovetail = pegs == "dovetail";
    let (shape, count) = if dovetail {
        (PegShape::Round, 1)
    } else {
        (shape, count)
    };
    let (placements, skipped) =
        pegs::place_holes(regions, shape, d / scale, count, min_wall / scale).map_err(
            |e| match e {
                PegError::DoesNotFit {
                    region,
                    widest_fit_mm,
                } => refuse(
                    "section_too_thin",
                    format!(
                        "Section {} is too thin for a {} hole with a {} wall: the widest hole \
                         that fits is {}. Use a smaller hole diameter or minimum wall, or \
                         Alignment: None.",
                        region + 1,
                        mf_spec::format_length(d),
                        mf_spec::format_length(min_wall),
                        mf_spec::format_length(widest_fit_mm * scale)
                    ),
                ),
            },
        )?;
    ctx.checkpoint()?;
    // World base points (on the bottom plane, and the top plane) and keys.
    let frame_below = regions.first().map(|r| r.frame);
    let frame_above = cut
        .above
        .as_ref()
        .and_then(|p| p.section_regions.first())
        .map(|r| r.frame);
    let mut holes = Vec::with_capacity(placements.len());
    for h in placements {
        let region = &regions[h.region];
        let frame = region.frame;
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
        let dovetail_plan = if dovetail {
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
                return Err(refuse(
                    "section_too_thin",
                    format!(
                        "Section {} is too small for a {} dovetail {} deep. Use a smaller hole \
                         diameter or depth, or Alignment: Dowels.",
                        h.region + 1,
                        mf_spec::format_length(d),
                        mf_spec::format_length(depth_below)
                    ),
                ));
            }
            Some(DovetailPlan {
                groove_half: span * scale,
                tail_half,
            })
        } else {
            None
        };
        holes.push(PlannedHole {
            placement: h,
            base_below,
            base_above,
            key,
            dovetail: dovetail_plan,
        });
    }
    Ok(PegPlan {
        kind: pegs.to_owned(),
        holes,
        skipped_regions: skipped,
        scale,
        dowel_diameter: dowel_d,
        dowel_length: dowel_len,
        attached_length: attached_len,
    })
}
