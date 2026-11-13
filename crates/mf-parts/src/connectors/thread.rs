//! Threads, "what's on the packet" presets (connector `thread` v1): the
//! user picks a metric coarse size (M1…M12) and a length, for example
//! `M3 x 12`; pitch and profile come from the standard ([`COARSE_PITCH`])
//! and are never shown unless "Advanced" is opened.
//!
//! **Real helical threads.** The generated geometry is a modelled helix,
//! not stacked rings: a bolt screws into its matching nut. The surface
//! follows the ISO 68-1 basic profile (60° flanks, crest and root
//! truncated flat so an FDM nozzle can resolve them) swept helically at
//! the preset's pitch ([`thread_surface`]). `bolt_hex`/`bolt_socket` cut
//! an external thread into the shaft; `nut`/`threaded_hole` cut an
//! internal thread into the bore (the same profile, offset outward by
//! the resolved fit clearance so the two interlock with a real gap,
//! never assumed). Each end gets a chamfered lead-in (the crest blends
//! down to the root over the first/last turn) so the thread starts
//! without a overhanging first crest. `clearance_hole`/`nut_trap`'s
//! bores stay plain cylinders: a bought bolt's shaft or a captured nut
//! doesn't thread into either, so modelling a helix there would add
//! nothing but risk.
//!
//! Every hole-shaped part (`threaded_hole`, `clearance_hole`,
//! `nut_trap`) is sized from `fit_clearance` (the Fit class, design
//! intent) plus `machine_tolerance` (the Machine profile's hole
//! undersize compensation), both recorded on the connector's own
//! parameters (D20) rather than assumed. `threaded_hole`/`nut` apply the
//! clearance radially, widening the internal thread's own profile
//! outward rather than just boring a wider plain cylinder, so the
//! resolved gap holds all along the flanks, not just at the crest.
//!
//! `threaded_hole`/`nut_trap` stay `Side::A`/`ShapeRole::Hole` tools
//! (D22): the boolean kernel that already applies every connector's
//! Subtract parts (`connector.join`, `mf-operations`) cuts the internal
//! thread into the target part; this module only has to produce the
//! tool's mesh.

use mf_geometry::Mesh;
use mf_spec::{OperationSpec, Params};
use std::sync::OnceLock;

use crate::connector::{ConnectorError, Side};
use crate::generator::{GenContext, Generator, PlacementHint, ShapePart};
use crate::shape::ShapeRole;

const SPEC_YAML: &str = include_str!("../../../../specs/connectors/thread.yaml");

fn spec() -> &'static OperationSpec {
    static SPEC: OnceLock<OperationSpec> = OnceLock::new();
    SPEC.get_or_init(|| {
        serde_saphyr::from_str(SPEC_YAML).expect("connectors/thread.yaml is a valid spec")
    })
}

/// The minimum segments per turn (owner requirement): resolution scales
/// with diameter above this floor ([`segments_for_diameter`]).
const MIN_SEGMENTS_PER_TURN: u32 = 32;

/// Metric coarse major diameter (mm) and pitch (mm) for each size on
/// the packet (owner requirements 2026-11-11): the one place the
/// standard's numbers live, used both by the plain bolt/nut outlines
/// and by the helix pitch.
pub const COARSE_PITCH: &[(&str, f64, f64)] = &[
    ("M1", 1.0, 0.25),
    ("M1.2", 1.2, 0.25),
    ("M1.6", 1.6, 0.35),
    ("M2", 2.0, 0.4),
    ("M2.5", 2.5, 0.45),
    ("M3", 3.0, 0.5),
    ("M4", 4.0, 0.7),
    ("M5", 5.0, 0.8),
    ("M6", 6.0, 1.0),
    ("M8", 8.0, 1.25),
    ("M10", 10.0, 1.5),
    ("M12", 12.0, 1.75),
];

/// The major diameter and pitch for a size on the packet (`"M3"`).
pub fn lookup(size: &str) -> Option<(f64, f64)> {
    COARSE_PITCH
        .iter()
        .find(|(s, _, _)| *s == size)
        .map(|(_, d, p)| (*d, *p))
}

fn refuse(code: &'static str, message: impl Into<String>) -> ConnectorError {
    ConnectorError::Failed {
        code,
        message: message.into(),
    }
}

/// Segments per turn for a thread of major diameter `d` (owner
/// requirement: scales with diameter, at least [`MIN_SEGMENTS_PER_TURN`]).
/// A coarser circle under-resolves a wide thread's crest flat into
/// visible facets; one segment per quarter-millimetre of circumference
/// keeps the chordal error well under print resolution at every size on
/// the packet, floored at the minimum so a tiny M1 thread is still
/// round enough to turn smoothly.
fn segments_for_diameter(d: f64) -> u32 {
    let circumference = std::f64::consts::PI * d;
    let scaled = (circumference / 0.25).ceil() as u32;
    scaled.max(MIN_SEGMENTS_PER_TURN)
}

/// A regular polygon of `n` sides, circumradius `r`, flat-up (a vertex
/// at angle 0).
fn polygon(n: u32, r: f64) -> Vec<[f64; 2]> {
    (0..n)
        .map(|i| {
            let a = std::f64::consts::TAU * i as f64 / n as f64;
            [r * a.cos(), r * a.sin()]
        })
        .collect()
}

/// A round profile (approximating the standard's circumradius with a
/// chordal error well under print resolution).
fn round(r: f64, segments: u32) -> Vec<[f64; 2]> {
    polygon(segments, r)
}

/// A prism of `profile` (CCW, in x,y) from `z0` to `z1`, around the z
/// axis (unlike `clasp`/`flap`'s x- or y-swept prisms: threads stand
/// upright for printing, like `builtin::revolve`).
fn prism_z(profile: &[[f64; 2]], z0: f64, z1: f64) -> Mesh {
    let n = profile.len() as u32;
    let mut positions = Vec::with_capacity(2 * profile.len() + 2);
    for &z in &[z0, z1] {
        positions.extend(profile.iter().map(|p| [p[0], p[1], z]));
    }
    let mut triangles = Vec::new();
    for i in 0..n {
        let j = (i + 1) % n;
        let (a, b) = (i, j);
        let (c, d) = (n + j, n + i);
        // CCW (x,y) profile swept along z: outward normal faces away
        // from the axis for [a,b,c] and [a,c,d] (same derivation as
        // `clasp::extrude_y`'s (x,z)-swept-along-y case: z plays the
        // role of the "up" axis there did).
        triangles.push([a, b, c]);
        triangles.push([a, c, d]);
    }
    // End caps: a centre-fan (the profiles here are always convex:
    // polygons and circles), bottom flipped (outward -z).
    let bottom_centre = positions.len() as u32;
    positions.push([0.0, 0.0, z0]);
    triangles.extend(fan_with_centre(0, n, bottom_centre, true));
    let top_centre = positions.len() as u32;
    positions.push([0.0, 0.0, z1]);
    triangles.extend(fan_with_centre(n, n, top_centre, false));
    Mesh::new(positions, triangles).expect("a prism with a convex profile is a valid mesh")
}

fn fan_with_centre(base: u32, n: u32, centre: u32, flip: bool) -> Vec<[u32; 3]> {
    (0..n)
        .map(|i| {
            let j = (i + 1) % n;
            if flip {
                [centre, base + j, base + i]
            } else {
                [centre, base + i, base + j]
            }
        })
        .collect()
}

/// How much a *radial* bump is foreshortened, at the flank, relative
/// to the true 3D minimum distance it actually opens up between a
/// mated bolt and nut — and so the scale-up `radial_clearance` needs so
/// the measured gap matches the fit class's resolved value, not just
/// the bump itself.
///
/// The thread's lateral surface is `P(theta, z) = (r(u) cos(theta),
/// r(u) sin(theta), z)` with `u = (z - theta*p/(2*pi)) / p` (the
/// helical phase, [`thread_wall`]'s own formula) and, on a flank,
/// `r(u) = r0 + slope*u` (piecewise-linear, [`ThreadProfile::radius_at`]).
/// Its surface normal is `(dP/dtheta) x (dP/dz)`; projecting the pure
/// radial direction `(cos theta, sin theta, 0)` onto that normal (at
/// `theta = 0`, by the surface's own rotational symmetry) gives, after
/// simplifying the cross product:
///
/// ```text
/// factor = 2*pi*p*r / sqrt(p^2*(4*pi^2*r^2 + slope^2) + 4*pi^2*r^2*slope^2)
/// ```
///
/// evaluated at `r = pitch_r` (the flank's own reference radius, where
/// the two mated profiles' flats also line up, D20) and `slope = H /
/// flank_width_in_u` (external and internal share the same flank
/// width, `5/16` of a turn, by the standard's own crest/root fractions
/// — [`ThreadProfile::new`]'s own comment on why the two interlock).
/// The flank is steep (H is large relative to one pitch's worth of
/// axial travel) and the helix's own circumferential sweep adds a
/// large tangential component to the normal, so this factor is small
/// (≈0.34, verified numerically against a from-scratch closest-point
/// search across every metric coarse preset): a plain radial clearance
/// delivers only about a third of itself as real clearance at the
/// flank, which is where a bolt and nut actually bear on each other.
fn radial_offset_compensation(d: f64, p: f64) -> f64 {
    let h = p * 3.0_f64.sqrt() / 2.0;
    let pitch_r = d / 2.0 - 3.0 * h / 8.0;
    let flank_width_u = 5.0 / 16.0;
    let slope = h / flank_width_u;
    let tau = std::f64::consts::TAU;
    let factor = tau * p * pitch_r
        / (p * p * (tau * tau * pitch_r * pitch_r + slope * slope)
            + tau * tau * pitch_r * pitch_r * slope * slope)
            .sqrt();
    1.0 / factor
}

/// The ISO 68-1 basic profile's key radii, derived from the major
/// diameter `d` and pitch `p` (ISO 724): `h` is the fundamental
/// (sharp-V, 60-degree) triangle's height; crest and root are each
/// truncated flat (never sharp, so they print) by the standard's own
/// fractions of it. `external` picks which radius is the thread's own
/// surface and which is the mating gap: an external (bolt) thread's
/// crest sits at the major diameter and its root is cut away from it;
/// an internal (nut/hole) thread's root sits at the major diameter
/// (plus any resolved clearance) and its crest is cut away *outward*
/// from it, into the hole — so the two always interlock at the same
/// nominal pitch radius, widened only by `radial_clearance`.
struct ThreadProfile {
    /// Pitch radius: `d/2` for an external thread, offset outward by
    /// `radial_clearance` for an internal one (D20: the fit class's
    /// clearance applied radially, not just at one surface). Not read
    /// by `radius_at` (only `crest_r`/`root_r` are), but kept and
    /// checked by `external_and_internal_profiles_share_the_pitch_radius`
    /// as the documented ISO 68-1 invariant that lets a bolt and its
    /// matching nut interlock.
    #[cfg_attr(not(test), allow(dead_code))]
    pitch_r: f64,
    /// Radius at the crest flat (the wider of the two local extremes
    /// for an internal thread's profile, since its "crest" points into
    /// the hole, away from the axis).
    crest_r: f64,
    /// Radius at the root flat.
    root_r: f64,
    /// Fraction of one pitch (0..1) the crest flat spans, centred on
    /// the start of each period.
    crest_fraction: f64,
    /// Fraction of one pitch the root flat spans, centred on the
    /// midpoint of each period.
    root_fraction: f64,
}

impl ThreadProfile {
    /// `d`: nominal major diameter. `p`: pitch. `radial_clearance`: the
    /// resolved fit gap (D20), added to the pitch radius so an internal
    /// thread is a true offset of the external one, not just a wider
    /// plain bore.
    fn new(d: f64, p: f64, external: bool, radial_clearance: f64) -> ThreadProfile {
        // H: the sharp-V (fundamental triangle) height, ISO 724.
        let h = p * 3.0_f64.sqrt() / 2.0;
        if external {
            let pitch_r = d / 2.0 - 3.0 * h / 8.0;
            ThreadProfile {
                pitch_r,
                crest_r: pitch_r + 3.0 * h / 8.0, // = d/2
                root_r: pitch_r - 5.0 * h / 8.0,
                crest_fraction: 1.0 / 8.0,
                root_fraction: 1.0 / 4.0,
            }
        } else {
            // Internal thread: same nominal pitch radius as the
            // matching external thread, offset outward by the resolved
            // clearance (compensated, see `radial_offset_compensation`:
            // on a helical flank, a purely radial bump is foreshortened
            // by the flank's own true surface normal, so the bump is
            // scaled up first to deliver the *actual* 3D gap the fit
            // class asked for); its own "root" (the hole's untouched
            // wall, further from the axis) corresponds to the external
            // thread's crest, and its "crest" (cut furthest into the
            // hole, closer to the axis) corresponds to the external
            // thread's root — the hole's negative space the bolt's
            // thread fills.
            let compensated = radial_clearance * radial_offset_compensation(d, p);
            let pitch_r = (d / 2.0 - 3.0 * h / 8.0) + compensated;
            ThreadProfile {
                pitch_r,
                crest_r: pitch_r - 5.0 * h / 8.0,
                root_r: pitch_r + 3.0 * h / 8.0,
                crest_fraction: 1.0 / 4.0,
                root_fraction: 1.0 / 8.0,
            }
        }
    }

    /// The truncated-trapezoid profile's radius at phase `u` (0..1, the
    /// fractional position within one pitch): flat at `crest_r` for
    /// `crest_fraction` of the period starting at `u = 0`, a straight
    /// 60-degree flank down to `root_r`, flat at `root_r` for
    /// `root_fraction` centred at `u = 0.5`, then a flank back up to
    /// `crest_r` at `u = 1`. Continuous and periodic (`radius_at(0) ==
    /// radius_at(1)`), so consecutive helical turns meet without a seam.
    fn radius_at(&self, u: f64) -> f64 {
        let half_crest = self.crest_fraction / 2.0;
        let half_root = self.root_fraction / 2.0;
        let flank_down_start = half_crest;
        let flank_down_end = 0.5 - half_root;
        let flank_up_start = 0.5 + half_root;
        let flank_up_end = 1.0 - half_crest;
        if u <= flank_down_start || u >= flank_up_end {
            self.crest_r
        } else if u < flank_down_end {
            let t = (u - flank_down_start) / (flank_down_end - flank_down_start);
            self.crest_r + (self.root_r - self.crest_r) * t
        } else if u <= flank_up_start {
            self.root_r
        } else {
            let t = (u - flank_up_start) / (flank_up_end - flank_up_start);
            self.root_r + (self.crest_r - self.root_r) * t
        }
    }
}

/// The open (uncapped) helical thread wall: every ring's positions (one
/// contiguous `Vec`, `rings + 1` rings of `segments` points each, ring
/// `i` starting at index `i * segments`) and the wall's own triangles,
/// swept around the z axis from `z0` upward, pitch `p`, major diameter
/// `d`. `external` picks the ISO 68-1 profile's orientation (see
/// [`ThreadProfile`]); `radial_clearance` widens an internal thread's
/// profile outward by the resolved fit gap.
///
/// `chamfer_ends` adds a lead-in at both ends: the crest blends down to
/// the root over (up to) the first and last turn, so ring 0 and the
/// last ring are both plain circles at the root radius — ready for a
/// flat fan cap, or (the bolt's head) a loft onto a different profile,
/// by a caller that owns the end caps itself. Owner requirement: a
/// bolt's exposed free end and a tapped hole's two mouths both get this
/// (so a driven-in thread starts without an unsupported first crest);
/// a nut's bore does not — it is assumed pre-engaged over its whole
/// (short) height, and chamfering both its own ends as well as the
/// mating bolt's would double up and locally pinch the resolved
/// clearance tighter than the fit class intends.
///
/// `segments` (at least [`MIN_SEGMENTS_PER_TURN`], scaled by diameter)
/// both resolves the round cross-section and samples the trapezoid wave
/// within each turn; the number of turns is rounded to a whole number
/// of pitches so ring 0 and the last ring land at the same angular
/// phase (needed for a flat end cap or a loft's own fan).
///
/// Returns `(positions, triangles, segments, rings, realized_length)`:
/// `realized_length` is the nearest whole number of turns to `length`
/// times the pitch, not always exactly `length`.
#[allow(clippy::type_complexity)]
fn thread_wall(
    d: f64,
    p: f64,
    length: f64,
    z0: f64,
    external: bool,
    radial_clearance: f64,
    chamfer_ends: bool,
) -> (Vec<[f64; 3]>, Vec<[u32; 3]>, u32, u32, f64) {
    let profile = ThreadProfile::new(d, p, external, radial_clearance);
    // The nominal major diameter `d`, not `d + 2 * radial_clearance`:
    // an internal thread's segment count must match its mating
    // external thread's exactly (same `d`, same formula), or the two
    // facetted (not perfectly round) helixes sample the true circle at
    // different angles and point counts. The faceting's own chordal
    // error (a facet's midpoint sits slightly inside the true circle)
    // then no longer cancels between the two surfaces, locally pinching
    // the measured gap tighter than the resolved clearance at whatever
    // angles the two polygons happen to misalign — `radial_clearance`
    // itself is a small fraction of `d`, so reusing `d` for the
    // facet-count formula still scales with the real diameter.
    let segments = segments_for_diameter(d);
    // The lead-in chamfer blends over one turn at each end, but never
    // more than a quarter of the thread's own length (so a very short
    // thread still has a full-depth thread, not one that's all chamfer).
    let lead_in_turns = if chamfer_ends {
        1.0_f64.min((length / p) / 4.0).max(0.0)
    } else {
        0.0
    };
    let turns = (length / p).round().max(1.0);
    let rings = (turns * segments as f64).round().max(segments as f64) as u32;
    let dz = (turns * p) / rings as f64;

    let mut positions: Vec<[f64; 3]> = Vec::with_capacity((rings as usize + 1) * segments as usize);
    let mut triangles: Vec<[u32; 3]> = Vec::new();
    let ring_base = |i: u32| i * segments;

    for i in 0..=rings {
        let z = z0 + i as f64 * dz;
        for s in 0..segments {
            let theta = std::f64::consts::TAU * s as f64 / segments as f64;
            // The thread's phase at this point is set by the helix
            // relationship between z and angle together, not by the
            // ring index alone: at a *fixed* z, the groove still winds
            // around, so the profile phase must vary with `theta` too
            // (the classic mistake this fixed — making every point in
            // a ring the same radius gives a stack of corrugated
            // rings, not a helix: true helicity means a horizontal
            // slice at constant z shows the groove spiralling, the
            // radius changing with angle at that fixed height). One
            // full revolution (`theta` 0 to tau) must correspond to
            // exactly one pitch of *effective* axial travel, so the
            // phase is `(z - theta/(tau) * p)` wrapped to one pitch.
            let u = ((z - theta / std::f64::consts::TAU * p) / p).rem_euclid(1.0);
            let mut r = profile.radius_at(u);
            // Chamfered lead-in/out: blend the crest down to the root
            // radius over the first/last `lead_in_turns` turns of
            // *this ring's own z* (a straight ramp in the fraction of
            // thread depth cut away, so the flank itself still reads
            // as a straight chamfer, applied per-point since the
            // helix's phase now varies within a ring too).
            if lead_in_turns > 0.0 {
                let turns_from_start = (z - z0) / p;
                let turns_from_end = (z0 + turns * p - z) / p;
                let edge_turns = turns_from_start.min(turns_from_end);
                if edge_turns < lead_in_turns {
                    let blend = (edge_turns / lead_in_turns).clamp(0.0, 1.0);
                    r = profile.root_r + (r - profile.root_r) * blend;
                }
            }
            positions.push([r * theta.cos(), r * theta.sin(), z]);
        }
    }
    for i in 0..rings {
        let a = ring_base(i);
        let b = ring_base(i + 1);
        for s in 0..segments {
            let s2 = (s + 1) % segments;
            // Outward-facing wall, same winding as `wall_z`.
            triangles.push([a + s, a + s2, b + s2]);
            triangles.push([a + s, b + s2, b + s]);
        }
    }
    let realized_length = rings as f64 * dz;
    (positions, triangles, segments, rings, realized_length)
}

/// A watertight helical thread surface, both ends closed by a flat fan
/// cap (see [`thread_wall`]). Chamfers both ends (used for
/// `threaded_hole`'s tapped hole, a true entry point at each mouth).
/// Returns the mesh and the realized length (not always exactly
/// `length`: see `thread_wall`).
pub(crate) fn thread_surface(
    d: f64,
    p: f64,
    length: f64,
    z0: f64,
    external: bool,
    radial_clearance: f64,
) -> (Mesh, f64) {
    let (mut positions, mut triangles, segments, rings, realized_length) =
        thread_wall(d, p, length, z0, external, radial_clearance, true);
    let ring_base = |i: u32| i * segments;
    let bottom_centre = positions.len() as u32;
    positions.push([0.0, 0.0, z0]);
    triangles.extend(fan_with_centre(ring_base(0), segments, bottom_centre, true));
    let top_centre = positions.len() as u32;
    positions.push([0.0, 0.0, z0 + realized_length]);
    triangles.extend(fan_with_centre(
        ring_base(rings),
        segments,
        top_centre,
        false,
    ));
    let mesh = Mesh::new(positions, triangles).expect("a helical thread surface is a valid mesh");
    (mesh, realized_length)
}

/// A solid from a profile at `z0` lofted to a different profile at
/// `z1`, both CCW and convex, closed by a fan cap at each end: the
/// transition band (where the two profiles may have different point
/// counts) is triangulated by connecting each point to its nearest
/// point on the other ring, the same coarse-but-always-simple approach
/// as [`bored_solid`]'s annulus caps.
fn loft(p0: &[[f64; 2]], z0: f64, p1: &[[f64; 2]], z1: f64, z1_top: f64) -> Mesh {
    let n0 = p0.len() as u32;
    let mut positions = Vec::new();
    let mut triangles = Vec::new();
    let base0 = positions.len() as u32;
    positions.extend(p0.iter().map(|p| [p[0], p[1], z0]));
    let ring0_top = positions.len() as u32;
    positions.extend(p0.iter().map(|p| [p[0], p[1], z1]));
    // The shaft's own side wall (z0 to z1, same profile both rings).
    for i in 0..n0 {
        let j = (i + 1) % n0;
        triangles.push([base0 + i, base0 + j, ring0_top + j]);
        triangles.push([base0 + i, ring0_top + j, ring0_top + i]);
    }
    let bottom_centre = positions.len() as u32;
    positions.push([0.0, 0.0, z0]);
    triangles.extend(fan_with_centre(base0, n0, bottom_centre, true));
    append_transition_and_head(
        &mut positions,
        &mut triangles,
        p0,
        ring0_top,
        p1,
        z1,
        z1_top,
    );
    Mesh::new(positions, triangles).expect("a loft between two convex profiles is a valid mesh")
}

/// The transition band from an existing ring of `n0` points (indices
/// `ring0_top..ring0_top + n0`, already in `positions`, at height
/// `z1`) to a different profile `p1` lofted up to `z1_top`, plus `p1`'s
/// own side wall and top fan cap — everything [`loft`] builds above
/// its own shared bottom ring, factored out so [`bolt`] can call it
/// directly on a helical thread wall's real top ring instead of a
/// freshly duplicated circle (so the thread and the head share
/// vertices exactly, one continuous manifold, rather than two solids
/// merely touching at a coincident face, which this codebase's exact
/// validator refuses). Appends new positions and triangles in place.
fn append_transition_and_head(
    positions: &mut Vec<[f64; 3]>,
    triangles: &mut Vec<[u32; 3]>,
    p0: &[[f64; 2]],
    ring0_top: u32,
    p1: &[[f64; 2]],
    z1: f64,
    z1_top: f64,
) {
    let (n0, n1) = (p0.len() as u32, p1.len() as u32);
    let base1 = positions.len() as u32;
    positions.extend(p1.iter().map(|p| [p[0], p[1], z1]));
    let top1 = positions.len() as u32;
    positions.extend(p1.iter().map(|p| [p[0], p[1], z1_top]));

    // The transition band at z1: ring0_top (p0's shape) to base1 (p1's
    // shape), each ring0_top point joined to its nearest base1 point.
    let nearest = |p: [f64; 2]| -> u32 {
        (0..n1)
            .min_by(|&a, &b| {
                let d = |i: u32| {
                    let q = p1[i as usize];
                    (q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2)
                };
                d(a).total_cmp(&d(b))
            })
            // allow-swallow: n1 = p1.len() is a round()/polygon() profile, never empty.
            .unwrap_or(0)
    };
    for i in 0..n0 {
        let j = (i + 1) % n0;
        let (ti, tj) = (nearest(p0[i as usize]), nearest(p0[j as usize]));
        triangles.push([ring0_top + i, ring0_top + j, base1 + ti]);
        if ti != tj {
            triangles.push([ring0_top + j, base1 + tj, base1 + ti]);
        }
    }
    // The head's own side wall (z1 to z1_top, p1's shape).
    for i in 0..n1 {
        let j = (i + 1) % n1;
        triangles.push([base1 + i, base1 + j, top1 + j]);
        triangles.push([base1 + i, top1 + j, top1 + i]);
    }
    let top_centre = positions.len() as u32;
    positions.push([0.0, 0.0, z1_top]);
    triangles.extend(fan_with_centre(top1, n1, top_centre, false));
}

/// A bolt with a hex (6-sided) or socket (round boss) head and a real
/// helical external thread (major diameter `d`, the standard's pitch)
/// of `length`.
fn bolt(d: f64, p: f64, length: f64, hex_head: bool) -> Mesh {
    let head_height = 0.7 * d;
    // The thread occupies [-length, 0] (the realized length may differ
    // slightly from the requested one, see `thread_wall`); the head
    // sits on top of it, lofted directly from the thread wall's own
    // top ring (the chamfered lead-in's root-radius circle) so the
    // whole bolt is one watertight solid sharing vertices at the
    // thread/head boundary — not a shaft and a head merged at a shared
    // plane (the `flap::bored_paddle` knuckle-and-wing lesson).
    let (mut positions, mut triangles, segments, rings, realized_length) =
        thread_wall(d, p, length, -length, true, 0.0, true);
    let ring_base = |i: u32| i * segments;
    // A round (socket) head reuses the thread's own segment count, so
    // its ring and the thread's top ring have exactly as many points
    // each: the transition band below connects them one-to-one, not by
    // nearest-point guessing (which silently drops points, and the
    // open edges that leaves, when the two rings' counts are far
    // apart — the thread's own count scales with the shaft diameter,
    // the head profile's would otherwise scale with the head's own,
    // larger diameter). A hex head's 6-point profile has no such risk
    // (nearest-point mapping from many points down to 6 never skips).
    let head_profile = if hex_head {
        let across_flats = 1.7 * d;
        let circumradius = across_flats / 2.0 / (std::f64::consts::PI / 6.0).cos();
        polygon(6, circumradius)
    } else {
        round(0.75 * d, segments)
    };
    let bottom_centre = positions.len() as u32;
    positions.push([0.0, 0.0, -length]);
    triangles.extend(fan_with_centre(ring_base(0), segments, bottom_centre, true));

    let z_head_base = -length + realized_length; // == 0 when realized_length == length
    let top_ring: Vec<[f64; 2]> = (ring_base(rings)..ring_base(rings) + segments)
        .map(|i| [positions[i as usize][0], positions[i as usize][1]])
        .collect();
    append_transition_and_head(
        &mut positions,
        &mut triangles,
        &top_ring,
        ring_base(rings),
        &head_profile,
        z_head_base,
        z_head_base + head_height,
    );
    Mesh::new(positions, triangles).expect("a threaded bolt is a valid mesh")
}

/// A hex nut: a hex prism with a real helical internal thread bored
/// through it, offset outward by the resolved fit clearance.
pub(crate) fn nut(d: f64, p: f64, radial_clearance: f64) -> Mesh {
    let height = 0.8 * d;
    let across_flats = 1.7 * d;
    let circumradius = across_flats / 2.0 / (std::f64::consts::PI / 6.0).cos();
    let outer = polygon(6, circumradius);
    bored_solid(&outer, d, p, height, false, radial_clearance)
}

/// An outer convex profile (prism from `z0 = 0` to `z1 = length`) with a
/// helical thread's own negative space bored through it — built
/// directly as one watertight solid (the outer wall, the thread wall
/// with its winding flipped to face inward, and an end cap annulus at
/// each end between the outer profile and the thread's own end ring)
/// rather than through the boolean kernel: the thread surface is
/// already the exact shape the bore needs (D22: the boolean kernel that
/// already applies every connector's `Subtract` part, `connector.join`,
/// does the actual cut into the target part this tool is sized for —
/// this only has to be the tool).
fn bored_solid(
    outer: &[[f64; 2]],
    d: f64,
    p: f64,
    length: f64,
    external: bool,
    radial_clearance: f64,
) -> Mesh {
    let n = outer.len() as u32;
    let mut positions = Vec::new();
    let mut triangles = Vec::new();

    // The thread's own wall first, winding flipped so its normal faces
    // into the solid (toward the axis) instead of outward. Never
    // chamfered (see `thread_wall`'s own comment): a nut's bore is
    // assumed pre-engaged over its whole short height, not a free
    // thread end. Built before the outer wall because the outer wall
    // must span exactly the bore's own realized length (the nearest
    // whole number of turns, not always exactly `length`): a mismatch
    // would leave the bore's last ring poking out past the outer
    // profile's top face (or short of it), both a visible defect and,
    // worse, a protruding thread tip nearer to a mating part than the
    // resolved clearance intends.
    let (wall_positions, wall_triangles, segments, rings, realized_length) =
        thread_wall(d, p, length, 0.0, external, radial_clearance, false);
    let outer_base = wall_z(outer, 0.0, realized_length, &mut positions, &mut triangles);

    let bore_base = positions.len() as u32;
    positions.extend(wall_positions.iter().copied());
    triangles.extend(
        wall_triangles
            .iter()
            .map(|t| [bore_base + t[2], bore_base + t[1], bore_base + t[0]]),
    );

    // End-cap annuli: the outer profile's ring to the thread's own ring
    // at each end (ring 0 at z = 0, ring `rings` at z = realized_length),
    // each outer point connected to its nearest thread-ring point, the
    // reverse of `loft`'s coarse-to-fine direction (here the fine ring
    // is the thread's own).
    let ring_base = |i: u32| bore_base + i * segments;
    for (ring_start, outer_off, flip) in [(ring_base(0), 0u32, true), (ring_base(rings), n, false)]
    {
        let ring: Vec<(u32, [f64; 2])> = (0..segments)
            .map(|s| {
                let idx = ring_start + s;
                let pos = positions[idx as usize];
                (idx, [pos[0], pos[1]])
            })
            .collect();
        let nearest_outer = |p: [f64; 2]| -> u32 {
            (0..n)
                .min_by(|&a, &b| {
                    let d = |i: u32| {
                        let q = outer[i as usize];
                        (q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2)
                    };
                    d(a).total_cmp(&d(b))
                })
                // allow-swallow: n = self.outer.len() is a round()/polygon() profile, never empty.
                .unwrap_or(0)
        };
        let nearest: Vec<u32> = ring.iter().map(|&(_, p)| nearest_outer(p)).collect();
        for i in 0..segments {
            let j = (i + 1) % segments;
            let (a, b) = (ring[i as usize].0, ring[j as usize].0);
            let c = outer_base + outer_off + nearest[i as usize];
            if flip {
                triangles.push([a, b, c]);
            } else {
                triangles.push([a, c, b]);
            }
            if nearest[i as usize] != nearest[j as usize] {
                let d = outer_base + outer_off + nearest[j as usize];
                if flip {
                    triangles.push([b, d, c]);
                } else {
                    triangles.push([b, c, d]);
                }
            }
        }
    }
    Mesh::new(positions, triangles)
        .expect("an outer prism with a helical bore and a coarse annulus cap is a valid mesh")
}

/// A ring's wall, swept along z from `z0` to `z1` (profile in x,y, CCW).
fn wall_z(
    ring: &[[f64; 2]],
    z0: f64,
    z1: f64,
    positions: &mut Vec<[f64; 3]>,
    triangles: &mut Vec<[u32; 3]>,
) -> u32 {
    let n = ring.len() as u32;
    let base = positions.len() as u32;
    for &z in &[z0, z1] {
        positions.extend(ring.iter().map(|p| [p[0], p[1], z]));
    }
    for i in 0..n {
        let j = (i + 1) % n;
        let (a, b) = (base + i, base + j);
        let (c, d) = (base + n + j, base + n + i);
        triangles.push([a, b, c]);
        triangles.push([a, c, d]);
    }
    base
}

pub struct Thread;

impl Generator for Thread {
    fn name(&self) -> &'static str {
        "Thread"
    }

    fn placement_hint(&self) -> PlacementHint {
        PlacementHint::Surface
    }

    fn quick_params(&self) -> &'static [&'static str] {
        &["size", "length"]
    }

    fn id(&self) -> &'static str {
        "thread"
    }

    fn version(&self) -> u32 {
        1
    }

    fn spec(&self) -> &'static OperationSpec {
        spec()
    }

    fn generate(
        &self,
        params: &Params,
        _ctx: &GenContext,
    ) -> Result<Vec<ShapePart>, ConnectorError> {
        let size = params
            .get_choice("size")
            .map_err(|e| refuse("invalid_parameter", e.to_string()))?;
        let length = params
            .get_float("length")
            .map_err(|e| refuse("invalid_parameter", e.to_string()))?;
        let shape = params
            .get_choice("shape")
            .map_err(|e| refuse("invalid_parameter", e.to_string()))?;
        let fit_clearance = params
            .get_float("fit_clearance")
            .map_err(|e| refuse("invalid_parameter", e.to_string()))?;
        let machine_tolerance = params
            .get_float("machine_tolerance")
            .map_err(|e| refuse("invalid_parameter", e.to_string()))?;

        let (major_d, pitch) = lookup(size).ok_or_else(|| {
            refuse(
                "unknown_thread_size",
                format!("'{size}' isn't a metric coarse size on the packet (M1 to M12)"),
            )
        })?;
        // The resolved gap (D20: fit class clearance plus machine
        // tolerance), diametral: a hole is widened by this much beyond
        // the bolt's own major diameter. Applied radially (half the
        // diametral gap) wherever a thread profile itself is offset,
        // so the resolved gap holds along the flanks, not just at one
        // surface.
        let hole_clearance = fit_clearance + machine_tolerance;
        let radial_clearance = hole_clearance.max(0.0) / 2.0;

        let (name, mesh, side, role): (&'static str, Mesh, Side, ShapeRole) = match shape {
            "bolt_hex" => (
                "bolt",
                bolt(major_d, pitch, length, true),
                Side::Neither,
                ShapeRole::Solid,
            ),
            "bolt_socket" => (
                "bolt",
                bolt(major_d, pitch, length, false),
                Side::Neither,
                ShapeRole::Solid,
            ),
            "nut" => (
                "nut",
                nut(major_d, pitch, radial_clearance),
                Side::Neither,
                ShapeRole::Solid,
            ),
            "threaded_hole" => {
                // The internal thread's own negative space: the fit
                // class never widens a tap-drill hole (tapping cuts its
                // own thread, it isn't a sliding fit) — only the
                // machine's own hole-undersize compensation does.
                let machine_radial = machine_tolerance.max(0.0) / 2.0;
                let (mesh, _realized_length) =
                    thread_surface(major_d, pitch, length, -length, false, machine_radial);
                ("threaded_hole", mesh, Side::A, ShapeRole::Hole)
            }
            "clearance_hole" => {
                // A loose hole for a bought bolt's shaft: never
                // threaded (the bolt's own thread does the work), so
                // this stays a plain cylinder, same as `nut_trap`'s
                // through-hole.
                let d = major_d + hole_clearance.max(0.0);
                let segments = segments_for_diameter(d);
                (
                    "clearance_hole",
                    prism_z(&round(d / 2.0, segments), -length, 1e-3),
                    Side::A,
                    ShapeRole::Hole,
                )
            }
            "nut_trap" => {
                let across_flats = 1.7 * major_d + hole_clearance.max(0.0);
                let circumradius = across_flats / 2.0 / (std::f64::consts::PI / 6.0).cos();
                let trap_depth = 0.8 * major_d + hole_clearance.max(0.0);
                let bolt_hole_d = major_d + hole_clearance.max(0.0);
                let segments = segments_for_diameter(bolt_hole_d);
                // One watertight tool (see `bolt`'s own comment on
                // `loft`): the round through-hole widens into the hex
                // trap, not two solids merged at a shared plane.
                (
                    "nut_trap",
                    loft(
                        &round(bolt_hole_d / 2.0, segments),
                        -length,
                        &polygon(6, circumradius),
                        -trap_depth,
                        1e-3,
                    ),
                    Side::A,
                    ShapeRole::Hole,
                )
            }
            other => {
                return Err(refuse(
                    "unknown_shape",
                    format!("'{other}' isn't a thread shape"),
                ));
            }
        };

        Ok(vec![ShapePart::new(name, role, side, mesh)])
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // tests may panic on failure
mod tests {
    use super::*;
    use mf_geometry::{Bvh, FaceId};

    fn assert_closed_solid(mesh: &Mesh, label: &str) {
        let ctx = mf_runtime::RunContext::none();
        let bvh = Bvh::build(mesh, &ctx).unwrap();
        let all: Vec<FaceId> = mesh.faces().map(|(f, _)| f).collect();
        mf_geometry::validate_solid(mesh, &all, &bvh, &ctx)
            .unwrap()
            .unwrap_or_else(|d| panic!("{label}: not a valid solid: {d}"));
    }

    #[test]
    fn every_coarse_size_has_the_packets_pitch() {
        let want = [
            ("M1", 0.25),
            ("M1.2", 0.25),
            ("M1.6", 0.35),
            ("M2", 0.4),
            ("M2.5", 0.45),
            ("M3", 0.5),
            ("M4", 0.7),
            ("M5", 0.8),
            ("M6", 1.0),
            ("M8", 1.25),
            ("M10", 1.5),
            ("M12", 1.75),
        ];
        for (size, pitch) in want {
            let (_, p) = lookup(size).unwrap_or_else(|| panic!("no entry for {size}"));
            assert_eq!(p, pitch, "{size}");
        }
    }

    #[test]
    fn bolt_and_nut_are_closed_solids() {
        let b = bolt(3.0, 0.5, 12.0, true);
        assert_closed_solid(&b, "bolt_hex");
        let s = bolt(3.0, 0.5, 12.0, false);
        assert_closed_solid(&s, "bolt_socket");
        let n = nut(3.0, 0.5, 0.2);
        assert_closed_solid(&n, "nut");
    }

    #[test]
    fn segments_scale_with_diameter_and_never_drop_below_the_floor() {
        assert_eq!(segments_for_diameter(1.0), MIN_SEGMENTS_PER_TURN);
        assert!(segments_for_diameter(12.0) > MIN_SEGMENTS_PER_TURN);
    }

    #[test]
    fn thread_profile_is_periodic_and_truncated() {
        let profile = ThreadProfile::new(3.0, 0.5, true, 0.0);
        assert_eq!(profile.radius_at(0.0), profile.radius_at(1.0));
        assert_eq!(profile.radius_at(0.0), profile.crest_r);
        assert_eq!(profile.radius_at(0.5), profile.root_r);
        // Truncated, never sharp: root is strictly inside the major
        // radius, never at or below zero.
        assert!(profile.root_r > 0.0);
        assert!(profile.root_r < profile.crest_r);
    }

    /// An external and an unclearanced internal thread for the same
    /// preset share the same nominal pitch radius (ISO 68-1): that's
    /// what lets a bolt and its matching nut interlock, with only the
    /// resolved fit clearance opening a real gap between the flanks.
    #[test]
    fn external_and_internal_profiles_share_the_pitch_radius() {
        let external = ThreadProfile::new(3.0, 0.5, true, 0.0);
        let internal = ThreadProfile::new(3.0, 0.5, false, 0.0);
        assert_eq!(external.pitch_r, internal.pitch_r);
        // A non-zero clearance widens the internal pitch radius by more
        // than the raw clearance (`radial_offset_compensation`, >1):
        // the flank's own surface normal is mostly axial/tangential,
        // not radial, so a plain radial bump under-delivers the fit
        // class's actual resolved gap unless scaled up first.
        let clearance = 0.1;
        let widened = ThreadProfile::new(3.0, 0.5, false, clearance);
        let compensation = radial_offset_compensation(3.0, 0.5);
        assert!(
            compensation > 1.0,
            "compensation should scale up: {compensation}"
        );
        assert_eq!(widened.pitch_r, internal.pitch_r + clearance * compensation);
    }
}
