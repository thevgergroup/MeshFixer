//! Connectors across a cut (the Split workflow's step 3, owner
//! 2026-11-11): `joint.dowel`, `joint.peg`, `joint.dovetail`,
//! `joint.magnet` and `joint.insert`, registered with the other built-in
//! connectors so `split.place_connectors` (and `connector.join`) build
//! them from the same registry.
//!
//! **Local frame** (every one of them): z is the cut's normal, from the
//! piece the connector was placed on (side A, "on") into the other piece
//! (side B, "other"); side A's cap is at `z = -gap/2`, side B's at
//! `z = +gap/2`; the connector's centre is at `x = y = 0`, and a
//! dovetail's tail runs along x. Placing a connector on a joint is a
//! rigid move of this frame (never a mirror), so the counterpart on the
//! other side is the same part seen from across the cut.
//!
//! - **dowel:** a hole in both pieces and a loose dowel (`Separate`).
//! - **peg:** a peg unioned onto A, a hole in B.
//! - **dovetail:** a tail unioned onto A, a groove through B.
//! - **magnet:** a pocket in both pieces.
//! - **insert:** a heat-set insert's pocket in A, the screw's clearance
//!   hole through B.
//!
//! Round features are polygons with their corners on the circle and a
//! chordal error of at most a tenth of the clearance, like the Split
//! alignment holes (D12).

use std::sync::OnceLock;

use mf_geometry::Mesh;
use mf_spec::{OperationSpec, Params};

use crate::connector::{ConnectorError, Side};
use crate::generator::{
    Anchor, FitProbe, Fix, GenCheck, GenContext, Generator, Handles, ParamPatch, PlacementHint,
    ShapePart,
};
use crate::shape::ShapeRole;

/// The flank angle of a dovetail, from the cut's normal (as the Split
/// alignment dovetail's, D12).
pub const DOVETAIL_ANGLE_DEGREES: f64 = 15.0;

fn refuse(code: &'static str, message: impl Into<String>) -> ConnectorError {
    ConnectorError::Failed {
        code,
        message: message.into(),
    }
}

fn float(params: &Params, name: &str) -> Result<f64, ConnectorError> {
    params
        .get_float(name)
        .map_err(|e| refuse("invalid_parameter", e.to_string()))
}

/// How many sides a round feature of radius `r` needs for a chordal
/// error of at most `tol` (at least 12).
fn round_sides(r: f64, tol: f64) -> usize {
    if r > 0.0 && tol > 0.0 && tol < r {
        ((std::f64::consts::PI / (1.0 - tol / r).acos()).ceil() as usize).clamp(12, 512)
    } else {
        12
    }
}

/// A regular polygon of radius `r` (corners on the circle), counter-
/// clockwise, with chordal error at most `tol` (at least 12 sides).
pub fn circle(r: f64, tol: f64) -> Vec<[f64; 2]> {
    n_gon(round_sides(r, tol) as u32, r)
}

/// A closed prism: the convex, counter-clockwise `profile` (in x, y)
/// from `z0` to `z1` (`z0 < z1`).
pub fn prism_z(profile: &[[f64; 2]], z0: f64, z1: f64) -> Mesh {
    let n = profile.len() as u32;
    let mut positions: Vec<[f64; 3]> = profile.iter().map(|p| [p[0], p[1], z0]).collect();
    positions.extend(profile.iter().map(|p| [p[0], p[1], z1]));
    let mut triangles = Vec::new();
    for i in 1..n - 1 {
        triangles.push([0, i + 1, i]);
        triangles.push([n, n + i, n + i + 1]);
    }
    for i in 0..n {
        let j = (i + 1) % n;
        triangles.push([i, j, n + j]);
        triangles.push([i, n + j, n + i]);
    }
    Mesh::new(positions, triangles).expect("a convex prism is a valid mesh")
}

/// A closed prism along x: the convex, counter-clockwise `section` in
/// (y, z), from `x0` to `x1`.
fn prism_x(section: &[[f64; 2]], x0: f64, x1: f64) -> Mesh {
    // A prism along z of (y, z) -> (x, y, z) = (z', y, ...): build it
    // along z, then turn (x, y, z) -> (z, x, y), a rotation (never a
    // mirror), so the winding stays outward.
    let along_z = prism_z(section, x0, x1);
    let (p, t) = along_z.to_arrays();
    let turned: Vec<[f64; 3]> = p.into_iter().map(|[a, b, c]| [c, a, b]).collect();
    Mesh::new(turned, t).expect("a turned prism is a valid mesh")
}

/// The chord tolerance for round features of diameter `d` with
/// `clearance` (diametral).
fn chord_tol(d: f64, clearance: f64) -> f64 {
    (clearance.abs() / 10.0).max(d / 1000.0).max(1e-4)
}

/// A dovetail's section, in (y, s) where `s` is the depth into side B
/// from its cap: `width` across at `s = 0`, widening by tan 15° per side
/// with depth, from `s0` to `s1`.
fn dovetail_section(width: f64, s0: f64, s1: f64, cap_b: f64) -> Vec<[f64; 2]> {
    let t = DOVETAIL_ANGLE_DEGREES.to_radians().tan();
    let w = |s: f64| width + 2.0 * s * t;
    // Counter-clockwise in (y, z): z = cap_b + s.
    vec![
        [-w(s0) / 2.0, cap_b + s0],
        [w(s0) / 2.0, cap_b + s0],
        [w(s1) / 2.0, cap_b + s1],
        [-w(s1) / 2.0, cap_b + s1],
    ]
}

/// A dovetail is placed by dragging a line across the cut face (owner
/// 2026-11-11, D23): its centre (`Placement.at`) and slide direction
/// (`Placement.angle`, the same local-x convention every other
/// connector's turn already uses) come from where the drag started and
/// ended; how far the groove reaches on each side of that centre, along
/// the slide direction, is recorded explicitly at placement time — never
/// re-derived from the cap outline at generate time, since the cap could
/// differ by the time `split.place_connectors` replays (the module docs'
/// "replay must be Exact").
///
/// `-x` reach is `groove_a`, `+x` reach `groove_b` (both half-lengths
/// from the centre, mm); `open_a`/`open_b` says whether each end is
/// **through** (`!= 0.0`: the recorded reach is already far past any
/// real cap, i.e. effectively unbounded, so the assembly can slide
/// apart from that side) or **stopped** (`== 0.0`: the groove ends
/// exactly at the recorded reach, short of the edge on purpose, so the
/// assembly can't be withdrawn that way). A placement from before this
/// existed has none of these fields; they default to one fixed length
/// each way (half of the old `groove_length` default) both through, so
/// an old save still slides freely both ways as it always did.
pub struct DovetailReach {
    pub a: f64,
    pub b: f64,
    pub open_a: bool,
    pub open_b: bool,
}

pub fn dovetail_reach(params: &Params) -> Result<DovetailReach, ConnectorError> {
    // `groove_length` is the pre-D23 single symmetric span; still read
    // as the fallback default (halved) for a placement saved before
    // `groove_a`/`groove_b` existed, so an old save keeps its old shape.
    let legacy = params.get_float("groove_length").unwrap_or(200.0) / 2.0;
    let a = params.get_float("groove_a").unwrap_or(legacy);
    let b = params.get_float("groove_b").unwrap_or(legacy);
    // Recorded as a bool now; a number (1 through, 0 stopped) by a
    // record made before the schema said so.
    let open = |k: &str| {
        params
            .get_bool(k)
            .or_else(|_| params.get_float(k).map(|v| v != 0.0))
            .unwrap_or(true) // allow-swallow: absent means through, as before open_a/open_b existed
    };
    let (open_a, open_b) = (open("open_a"), open("open_b"));
    Ok(DovetailReach {
        a: a.max(0.0),
        b: b.max(0.0),
        open_a,
        open_b,
    })
}

/// A dowel or peg's cross-section (D22, anti-rotation): round turns
/// freely in its hole, so a joint with only one round dowel or peg can
/// rotate about it; the other three shapes each resist turning on their
/// own. Encoded in `Placement.params` as `shape` (0..=3, see
/// [`Shape::from_code`]) and, for `NSided`, `sides` (3..=12, default 6 —
/// a hexagon, the usual printable compromise between a strong flat-to-
/// flat grip and a mostly-round hole).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Round,
    Square,
    /// A regular polygon with `sides` corners on the circle (3..=12; 6
    /// by default, i.e. a hex).
    NSided {
        sides: u32,
    },
    /// A "D" profile: a circle with one flat, same as `mesh.split_plane`'s
    /// own keyed alignment holes (`split::pegs::PegShape::Keyed`).
    Keyed,
}

impl Shape {
    /// Decodes `shape` (0 round, 1 square, 2 N-sided, 3 keyed) and
    /// `sides` (used only for N-sided), as recorded in a placement's
    /// params. An out-of-range code reads as Round rather than refusing:
    /// the fields are plain f64s with no enum validation of their own,
    /// so this is the parse that can't fail (like every other numeric
    /// param here), matching how the rest of this module favours a safe
    /// default over a hard error on a stray value.
    pub fn from_code(shape: f64, sides: f64) -> Shape {
        match shape.round() as i64 {
            1 => Shape::Square,
            2 => Shape::NSided {
                sides: (sides.round() as i64).clamp(3, 12) as u32,
            },
            3 => Shape::Keyed,
            _ => Shape::Round,
        }
    }

    /// The numeric code this shape records as its `shape` param.
    pub fn code(self) -> f64 {
        match self {
            Shape::Round => 0.0,
            Shape::Square => 1.0,
            Shape::NSided { .. } => 2.0,
            Shape::Keyed => 3.0,
        }
    }
}

/// A regular polygon of radius `r` (corners on the circle), `n` sides,
/// counter-clockwise, first corner on +x. [`circle`] is this at the side
/// count a round feature needs for its chordal tolerance; an anti-
/// rotation N-sided dowel or peg ([`Shape::NSided`]) calls it directly
/// with its own, much smaller, `sides` (3..=12).
pub(crate) fn n_gon(n: u32, r: f64) -> Vec<[f64; 2]> {
    let n = n.max(3);
    (0..n)
        .map(|i| {
            let a = std::f64::consts::TAU * i as f64 / n as f64;
            [r * a.cos(), r * a.sin()]
        })
        .collect()
}

/// A square of side `side`, centred, counter-clockwise.
fn square_profile(side: f64) -> Vec<[f64; 2]> {
    let h = side / 2.0;
    vec![[-h, -h], [h, -h], [h, h], [-h, h]]
}

/// A "D" profile: a circle of radius `r` with the part beyond `flat`
/// (along +x) cut off (as `mesh.split_plane`'s own keyed holes,
/// `split::pegs::profile`'s `PegShape::Keyed` arm).
pub(crate) fn keyed_profile(r: f64, flat: f64, tol: f64) -> Vec<[f64; 2]> {
    let n = round_sides(r, tol);
    let half = (flat / r).clamp(-1.0, 1.0).acos();
    let mut out: Vec<[f64; 2]> = (0..=n)
        .map(|i| {
            let a = half + (std::f64::consts::TAU - 2.0 * half) * i as f64 / n as f64;
            [r * a.cos(), r * a.sin()]
        })
        .collect();
    out.dedup_by(|a, b| (a[0] - b[0]).abs() < 1e-12 && (a[1] - b[1]).abs() < 1e-12);
    out
}

/// The full-size hole (or pocket) profile for `shape` at diameter `d`:
/// `Round` is [`circle`]; `Square` a square of side `d` and `Keyed` a
/// "D" of radius `d/2` with a flat at `0.6` of the radius, both exactly
/// as `split::pegs::profile` builds its own alignment holes of the same
/// shapes; `NSided` a regular polygon of `sides` corners on that same
/// circle.
fn shape_profile(shape: Shape, d: f64, tol: f64) -> Vec<[f64; 2]> {
    let r = d / 2.0;
    match shape {
        Shape::Round => circle(r, tol),
        Shape::Square => square_profile(d),
        Shape::NSided { sides } => n_gon(sides, r),
        Shape::Keyed => keyed_profile(r, 0.6 * r, tol),
    }
}

/// A round hole's outline with the profile its params ask for (fix item
/// 9, D24): exactly [`circle`] when Round, a teardrop or flat-top over
/// the same circle otherwise, its point facing the recorded `up_x`/
/// `up_y` (world up seen from the hole). A placement made before the
/// hole profile existed has no such params and stays round.
fn round_hole(params: &Params, r: f64, tol: f64) -> Vec<[f64; 2]> {
    let profile = crate::shape::chosen_profile(params);
    let up = [
        params.get_float("up_x").unwrap_or(0.0),
        params.get_float("up_y").unwrap_or(1.0),
    ];
    let overhang = params.get_float("overhang").unwrap_or(45.0);
    super::hole_profile::hole_outline(profile, r, up, overhang, tol)
}

/// A dowel or peg's hole: its shape's own outline, or for a round one
/// the hole profile its params ask for.
fn hole_outline_for(params: &Params, shape: Shape, d: f64, tol: f64) -> Vec<[f64; 2]> {
    match shape {
        Shape::Round => round_hole(params, d / 2.0, tol),
        other => shape_profile(other, d, tol),
    }
}

/// The pin (dowel or peg) profile for `shape`: the hole's own profile at
/// `d − clearance` (diametral), so every shape narrows by exactly the
/// same clearance a round pin would.
fn shape_pin_profile(shape: Shape, d: f64, clearance: f64, tol: f64) -> Vec<[f64; 2]> {
    shape_profile(shape, d - clearance, tol)
}

/// What each kind's parameters say, read once.
struct Sizes {
    d: f64,
    depth_on: f64,
    depth_other: f64,
    clearance: f64,
    allowance: f64,
    gap: f64,
}

fn sizes(params: &Params, with_allowance: bool) -> Result<Sizes, ConnectorError> {
    let s = Sizes {
        d: float(params, "diameter")?,
        depth_on: float(params, "depth_on")?,
        depth_other: float(params, "depth_other")?,
        clearance: float(params, "clearance")?,
        allowance: if with_allowance {
            float(params, "length_allowance")?
        } else {
            0.0
        },
        gap: float(params, "gap")?,
    };
    if s.clearance >= s.d {
        return Err(refuse(
            "clearance_too_wide",
            format!(
                "The clearance ({:.2} mm) is as wide as the hole ({:.2} mm), so there'd be \
                 nothing left; make the hole wider or the clearance smaller.",
                s.clearance, s.d
            ),
        ));
    }
    Ok(s)
}

fn part(name: &'static str, mesh: Mesh, side: Side, role: ShapeRole) -> ShapePart {
    ShapePart::new(name, role, side, mesh)
}

/// The dowel's length: `dowel_length`, or (0) the two depths plus the
/// gap less the allowance.
pub fn dowel_length(params: &Params) -> Result<f64, ConnectorError> {
    let s = sizes(params, true)?;
    let set = float(params, "dowel_length")?;
    Ok(if set > 0.0 {
        set
    } else {
        s.depth_on + s.depth_other + s.gap - s.allowance
    })
}

/// The longest a dowel can be and still seat: both holes' depths plus
/// the gap.
pub fn dowel_room(params: &Params) -> Result<f64, ConnectorError> {
    let s = sizes(params, true)?;
    Ok(s.depth_on + s.depth_other + s.gap)
}

/// The dowel or peg shape a placement's `shape`/`sides` params record
/// ([`Shape::from_code`]); `0.0`/`6.0` (Round, 6 sides) for a connector
/// that doesn't have these fields in its spec at all (every joint
/// connector but dowel and peg) or a placement made before this shape
/// picker existed, so an old saved connector keeps reading as the round
/// pin it always was.
fn shape_of(params: &Params) -> Shape {
    let shape = match params.get_choice("shape") {
        Ok(word) => SHAPE_WORDS.iter().position(|w| *w == word).unwrap_or(0) as f64, // allow-swallow: the spec only allows these words
        // A record made when the shape was a number (its code).
        Err(_) => params.get_float("shape").unwrap_or(0.0), // allow-swallow: absent means round, as before the shape picker existed
    };
    let sides = params.get_float("sides").unwrap_or(6.0);
    Shape::from_code(shape, sides)
}

/// The dowel and peg `shape` choice's words, in [`Shape::code`] order.
pub const SHAPE_WORDS: [&str; 4] = ["round", "square", "n_sided", "keyed"];

fn generate(id: &str, params: &Params) -> Result<Vec<ShapePart>, ConnectorError> {
    match id {
        "joint.dowel" => {
            let s = sizes(params, true)?;
            let tol = chord_tol(s.d, s.clearance);
            let shape = shape_of(params);
            let (a, b) = (-s.gap / 2.0, s.gap / 2.0);
            let over = s.d;
            let hole = hole_outline_for(params, shape, s.d, tol);
            let len = dowel_length(params)?;
            if len <= 0.0 {
                return Err(refuse(
                    "dowel_too_short",
                    "The holes are too shallow for a dowel after the length allowance; make \
                     them deeper.",
                ));
            }
            let mid = (s.depth_other - s.depth_on) / 2.0;
            Ok(vec![
                part(
                    "hole",
                    prism_z(&hole, a - s.depth_on, a + over),
                    Side::A,
                    ShapeRole::Hole,
                ),
                part(
                    "counterpart hole",
                    prism_z(&hole, b - over, b + s.depth_other),
                    Side::B,
                    ShapeRole::Hole,
                ),
                part(
                    "dowel",
                    prism_z(
                        &shape_pin_profile(shape, s.d, s.clearance, tol),
                        mid - len / 2.0,
                        mid + len / 2.0,
                    ),
                    Side::Neither,
                    ShapeRole::Solid,
                ),
            ])
        }
        "joint.peg" => {
            let s = sizes(params, true)?;
            let tol = chord_tol(s.d, s.clearance);
            let shape = shape_of(params);
            let (a, b) = (-s.gap / 2.0, s.gap / 2.0);
            let stick = s.depth_other - s.allowance;
            if stick <= 0.0 {
                return Err(refuse(
                    "peg_too_short",
                    "The hole is too shallow for a peg after the length allowance; make it \
                     deeper.",
                ));
            }
            Ok(vec![
                part(
                    "peg",
                    prism_z(
                        &shape_pin_profile(shape, s.d, s.clearance, tol),
                        a - s.depth_on,
                        b + stick,
                    ),
                    Side::A,
                    ShapeRole::Solid,
                ),
                part(
                    "counterpart hole",
                    prism_z(
                        &hole_outline_for(params, shape, s.d, tol),
                        b - s.d,
                        b + s.depth_other,
                    ),
                    Side::B,
                    ShapeRole::Hole,
                ),
            ])
        }
        "joint.dovetail" => {
            let s = sizes(params, true)?;
            let length = float(params, "length")?;
            let reach = dovetail_reach(params)?;
            // The groove must reach at least as far as the tail itself
            // on each side, whatever was recorded (a tail that outgrew
            // its groove after an edit still slides, it just wouldn't
            // make sense to groove less than the tail that has to pass
            // through it).
            let (ga, gb) = (reach.a.max(length / 2.0), reach.b.max(length / 2.0));
            let (a, b) = (-s.gap / 2.0, s.gap / 2.0);
            let stick = s.depth_other - s.allowance;
            if stick <= 0.0 {
                return Err(refuse(
                    "groove_too_shallow",
                    "The groove is too shallow for a tail after the length allowance; make it \
                     deeper.",
                ));
            }
            let t = DOVETAIL_ANGLE_DEGREES.to_radians().tan();
            // The groove starts a little outside side B (only B is cut);
            // the tail's root is sunk into side A, narrowing with the
            // flanks, never below half its width at the cut.
            let groove = dovetail_section(s.d, -s.d / 4.0, s.depth_other, b);
            let root = (b - a + s.depth_on).min(0.25 * (s.d - s.clearance) / t);
            let tail = dovetail_section(s.d - s.clearance, -root, stick, b);
            Ok(vec![
                part(
                    "tail",
                    prism_x(&tail, -length / 2.0, length / 2.0),
                    Side::A,
                    ShapeRole::Solid,
                ),
                part(
                    "counterpart groove",
                    prism_x(&groove, -ga, gb),
                    Side::B,
                    ShapeRole::Hole,
                ),
            ])
        }
        "joint.magnet" => {
            let s = sizes(params, false)?;
            let tol = chord_tol(s.d, s.clearance);
            let (a, b) = (-s.gap / 2.0, s.gap / 2.0);
            let pocket = round_hole(params, (s.d + s.clearance) / 2.0, tol);
            Ok(vec![
                part(
                    "pocket",
                    prism_z(&pocket, a - s.depth_on, a + s.d),
                    Side::A,
                    ShapeRole::Hole,
                ),
                part(
                    "counterpart pocket",
                    prism_z(&pocket, b - s.d, b + s.depth_other),
                    Side::B,
                    ShapeRole::Hole,
                ),
            ])
        }
        "joint.insert" => {
            let s = sizes(params, false)?;
            let screw = float(params, "screw_diameter")?;
            let tol = chord_tol(s.d, s.clearance);
            let (a, b) = (-s.gap / 2.0, s.gap / 2.0);
            Ok(vec![
                part(
                    "insert pocket",
                    prism_z(&round_hole(params, s.d / 2.0, tol), a - s.depth_on, a + s.d),
                    Side::A,
                    ShapeRole::Hole,
                ),
                part(
                    "counterpart screw hole",
                    prism_z(
                        &round_hole(params, (screw + s.clearance) / 2.0, tol),
                        b - s.d,
                        b + s.depth_other,
                    ),
                    Side::B,
                    ShapeRole::Hole,
                ),
            ])
        }
        _ => Err(refuse(
            "unknown_connector",
            format!("'{id}' isn't a joint connector"),
        )),
    }
}

/// The mating pair the fit check measures (the exact BVH minimum
/// distance, `crate::check_fit`): the hole (or groove) and the dowel,
/// peg or tail that goes in it, each trimmed back from the hole's mouth
/// and floor so only the flanks are compared (a probe flush with the
/// mouth would touch there whatever the clearance). `None` for a
/// connector with no mating part of its own (magnet, insert: their fit
/// is against a bought part).
pub fn fit_probe(id: &str, params: &Params) -> Result<Option<FitProbe>, ConnectorError> {
    let parts = match id {
        "joint.dowel" | "joint.peg" | "joint.dovetail" => generate(id, params)?,
        _ => return Ok(None),
    };
    let s = sizes(params, true)?;
    let b = s.gap / 2.0;
    let margin = s.clearance.abs().max(0.1);
    let hole = parts
        .iter()
        .find(|p| p.attach_to == Side::B && p.role == ShapeRole::Hole)
        .map(|p| p.mesh.clone());
    let Some(hole) = hole else { return Ok(None) };
    // Inside side B's hole: from its mouth to short of its floor.
    let (z0, z1) = (
        b + margin,
        b + s.depth_other - s.allowance.max(margin) - margin,
    );
    if z1 <= z0 {
        return Ok(None);
    }
    let tol = chord_tol(s.d, s.clearance);
    let pin = match id {
        "joint.dovetail" => {
            let length = float(params, "length")?;
            let tail = dovetail_section(s.d - s.clearance, z0 - b, z1 - b, b);
            prism_x(&tail, -length / 2.0 + margin, length / 2.0 - margin)
        }
        _ => prism_z(
            &shape_pin_profile(shape_of(params), s.d, s.clearance, tol),
            z0,
            z1,
        ),
    };
    let label = match id {
        "joint.dowel" => "Dowel to hole",
        "joint.peg" => "Peg to hole",
        _ => "Tail to groove",
    };
    Ok(Some(FitProbe {
        label: label.to_owned(),
        hole,
        pin,
    }))
}

/// A joint connector's own checks: a dowel no longer than its two holes;
/// a stopped dovetail end that falls short of the cut face's edge (it
/// can't be slid in or out from there, D23), measured through `ctx`'s
/// targets.
fn checks(id: &str, params: &Params, ctx: &GenContext) -> Vec<GenCheck> {
    let mut out = Vec::new();
    match id {
        "joint.dowel" => {
            if let (Ok(len), Ok(room), Ok(allowance)) = (
                dowel_length(params),
                dowel_room(params),
                float(params, "length_allowance"),
            ) && len > room - 1e-9
            {
                out.push(GenCheck {
                    id: "dowel_length".into(),
                    message: format!(
                        "The dowel ({len:.1} mm) is longer than its two holes together \
                         ({room:.1} mm), so the pieces won't close."
                    ),
                    fix: Some(Fix {
                        label: "Shorten the dowel".into(),
                        patch: ParamPatch::float("dowel_length", room - allowance.max(0.1)),
                    }),
                });
            }
        }
        "joint.dovetail" => {
            let Ok(reach) = dovetail_reach(params) else {
                return out;
            };
            // The groove is cut into the other piece (side B): its cut
            // face is the boundary a stopped end must reach.
            for (open, len, dir, end, key) in [
                (reach.open_a, reach.a, [-1.0, 0.0], "a", "groove_a"),
                (reach.open_b, reach.b, [1.0, 0.0], "b", "groove_b"),
            ] {
                if open {
                    continue;
                }
                let Some(to_edge) = ctx.boundary_along(Side::B, dir) else {
                    continue;
                };
                if len < to_edge - 1e-6 {
                    out.push(GenCheck {
                        id: format!("dovetail_stopped.{end}"),
                        message: "Can't slide together: extend it to the edge".to_owned(),
                        // A touch past the edge, never short of it however
                        // the ray's rounding falls, so it can't re-fire.
                        fix: Some(Fix {
                            label: "Extend to the edge".into(),
                            patch: ParamPatch::float(key, to_edge + 0.5),
                        }),
                    });
                }
            }
        }
        _ => {}
    }
    out
}

/// How a joint connector holds the joint against turning (D22).
fn anchor(id: &str, params: &Params) -> Anchor {
    match id {
        "joint.dovetail" | "joint.insert" => Anchor::Resists,
        "joint.dowel" | "joint.peg" => match shape_of(params) {
            Shape::Round => Anchor::RoundPin {
                fix: Fix {
                    label: "Make square".into(),
                    patch: ParamPatch::one("shape", mf_spec::ParamValue::Text("square".into())),
                },
            },
            _ => Anchor::Resists,
        },
        _ => Anchor::None,
    }
}

/// A dovetail dragged as a line (D23): its groove reaches from the line's
/// centre to the other piece's cut face on each side, through (recorded
/// well past the edge) when the drag reached it, stopped at exactly the
/// dragged length when it fell short on purpose.
///
/// "Through" needs a reach that's unambiguously past the real edge,
/// measured by `ctx` (consolidation plan C18): a flat millimetre
/// constant assumed every piece was under a metre, so a dovetail dragged
/// edge-to-edge on a larger piece (a metre, say, or three) could still
/// stop short of actually clearing it once its own local-frame geometry
/// moved past the overshoot. [`OVER_THE_EDGE_FRACTION`] is relative to
/// `edge` itself instead — the distance `ctx` just measured to the
/// target's own cut-face boundary — so the margin always scales with the
/// piece; [`OVER_THE_EDGE_MIN`] keeps it from vanishing on a tiny one.
fn line_params(half: f64, ctx: &GenContext) -> ParamPatch {
    const OVER_THE_EDGE_FRACTION: f64 = 0.5;
    const OVER_THE_EDGE_MIN: f64 = 10.0;
    const SLIVER: f64 = 10.0;
    let mut patch = ParamPatch::default();
    for (dir, reach_key, open_key) in [
        ([-1.0, 0.0], "groove_a", "open_a"),
        ([1.0, 0.0], "groove_b", "open_b"),
    ] {
        let (reach, open) = match ctx.boundary_along(Side::B, dir) {
            Some(edge) if half >= edge - 1e-6 => {
                let over_the_edge = (edge * OVER_THE_EDGE_FRACTION).max(OVER_THE_EDGE_MIN);
                (edge + over_the_edge, true)
            }
            _ => (half.max(SLIVER), false),
        };
        patch.set.insert(
            reach_key.into(),
            mf_spec::ParamValue::Float((reach * 10.0).round() / 10.0),
        );
        patch.set.insert(
            open_key.into(),
            mf_spec::ParamValue::Float(if open { 1.0 } else { 0.0 }),
        );
    }
    patch
}

/// A record made when `shape` was a number (its [`Shape::code`]) or
/// `open_a`/`open_b` were (1 through, 0 stopped): the words and bools
/// the schema takes now.
fn upgrade(raw: &mut std::collections::BTreeMap<String, String>) {
    // allow-swallow: only a number needs upgrading; a word is already current
    if let Some(code) = raw.get("shape").and_then(|v| v.parse::<f64>().ok()) {
        let word = SHAPE_WORDS[(code.round().clamp(0.0, 3.0)) as usize];
        raw.insert("shape".into(), word.into());
    }
    for k in ["open_a", "open_b"] {
        // allow-swallow: only a number needs upgrading; a bool is already current
        if let Some(v) = raw.get(k).and_then(|v| v.parse::<f64>().ok()) {
            raw.insert(k.into(), (v != 0.0).to_string());
        }
    }
}

/// A placement saved before `groove_a`/`groove_b` existed (D23) has only
/// the old single symmetric `groove_length`: split in two, before
/// validation fills `groove_a`/`groove_b` with their own (unrelated)
/// defaults, so an old save keeps its old shape.
fn upgrade_dovetail(raw: &mut std::collections::BTreeMap<String, String>) {
    if raw.contains_key("groove_a") || raw.contains_key("groove_b") {
        return;
    }
    let half = raw
        .get("groove_length")
        .and_then(|v| v.parse::<f64>().ok()) // allow-swallow: an unreadable legacy length reads as the old default, as it always did
        .unwrap_or(200.0)
        / 2.0;
    raw.insert("groove_a".into(), half.to_string());
    raw.insert("groove_b".into(), half.to_string());
}

macro_rules! joint_connector {
    ($name:ident, $id:literal, $label:literal, $file:literal, $hint:expr, $size:literal) => {
        pub struct $name;

        impl Generator for $name {
            fn id(&self) -> &'static str {
                $id
            }

            fn name(&self) -> &'static str {
                $label
            }

            fn version(&self) -> u32 {
                1
            }

            fn spec(&self) -> &'static OperationSpec {
                static SPEC: OnceLock<OperationSpec> = OnceLock::new();
                SPEC.get_or_init(|| {
                    serde_saphyr::from_str(include_str!(concat!(
                        "../../../../specs/connectors/",
                        $file
                    )))
                    .expect(concat!("connectors/", $file, " is a valid spec"))
                })
            }

            fn generate(
                &self,
                params: &Params,
                _ctx: &GenContext,
            ) -> Result<Vec<ShapePart>, ConnectorError> {
                generate($id, params)
            }

            fn placement_hint(&self) -> PlacementHint {
                $hint
            }

            fn deprecated(&self) -> bool {
                // The dovetail moved to `dovetail.slide` (owner
                // 2026-11-12); this one stays for old joints and logs.
                $id == "joint.dovetail"
            }

            fn checks(&self, params: &Params, ctx: &GenContext) -> Vec<GenCheck> {
                checks($id, params, ctx)
            }

            fn fit_probe(&self, params: &Params) -> Result<Option<FitProbe>, ConnectorError> {
                fit_probe($id, params)
            }

            fn anchor(&self, params: &Params) -> Anchor {
                anchor($id, params)
            }

            fn checks_breakout(&self) -> bool {
                // A groove runs out of its piece on purpose.
                $id != "joint.dovetail"
            }

            fn handles(&self) -> Handles {
                Handles {
                    size: Some($size),
                    depth: Some("depth_on"),
                }
            }

            fn line_params(&self, half: f64, ctx: &GenContext) -> ParamPatch {
                if $id == "joint.dovetail" {
                    line_params(half, ctx)
                } else {
                    ParamPatch::default()
                }
            }

            fn upgrade_params(&self, raw: &mut std::collections::BTreeMap<String, String>) {
                upgrade(raw);
                if $id == "joint.dovetail" {
                    upgrade_dovetail(raw);
                }
            }
        }
    };
}

joint_connector!(
    Dowel,
    "joint.dowel",
    "Dowel",
    "joint.dowel.yaml",
    PlacementHint::Point,
    "diameter"
);
joint_connector!(
    Peg,
    "joint.peg",
    "Peg",
    "joint.peg.yaml",
    PlacementHint::Point,
    "diameter"
);
joint_connector!(
    Dovetail,
    "joint.dovetail",
    "Dovetail",
    "joint.dovetail.yaml",
    PlacementHint::DragLine,
    "length"
);
joint_connector!(
    Magnet,
    "joint.magnet",
    "Magnet",
    "joint.magnet.yaml",
    PlacementHint::Point,
    "diameter"
);
joint_connector!(
    Insert,
    "joint.insert",
    "Insert",
    "joint.insert.yaml",
    PlacementHint::Point,
    "diameter"
);
