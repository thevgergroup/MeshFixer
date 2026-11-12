//! The teardrop hole profile (owner 2026-11-11, D24): a 2D cross-section
//! addition shared by every circular hole or pocket a connector or the
//! Split workflow generates (`joint.*`'s `circle()`, `split::pegs`'s own
//! hole profile) when the hole's axis runs more sideways than vertical
//! in the piece's print orientation, so it won't need support or sag at
//! its own unsupported top.
//!
//! **The geometry** (a standard FDM teardrop construction): the round
//! part of the hole is untouched — `Round`, `Teardrop` and `FlatTop` all
//! start from exactly the same circle of radius `r`, and `Teardrop`/
//! `FlatTop` only ever add material outside it, never shrink or distort
//! it, so whatever bears against the circular region (a dowel, a screw,
//! a pin) sees the same fit regardless of profile. The overhang angle
//! `θ`, from vertical (the point's own direction, `up` in
//! [`hole_outline`]'s own 2D frame), controls where the circle's own
//! round arc gives way to the shape's
//! straight sides: the two tangent points sit at angle `θ` either side
//! of vertical, at `(±r sinθ, r cosθ)` (a radius line tilted `θ` off the
//! point's direction, by `θ`'s own definition as the angle the surface
//! normal there makes with vertical). From there:
//! - **Teardrop**: the two tangent lines run straight up to meet on the
//!   axis at the apex, `(0, r / cosθ)` — the line tangent to the circle
//!   at `(r sinθ, r cosθ)` runs in direction `(-cosθ, sinθ)` (perpendic-
//!   ular to that point's own radius); solving for where it crosses the
//!   axis (`x = 0`) gives `t = r tanθ` along the tangent, landing at
//!   `y = r cosθ + t sinθ = r cosθ + r sin²θ / cosθ = r / cosθ` (using
//!   `cos²θ + sin²θ = 1`). At `θ = 0` this is `r`, the circle's own top
//!   (no teardrop at all); at the usual 45° default it's `r√2`.
//! - **FlatTop**: the same two tangent points, but instead of meeting at
//!   an apex, cut off flat at `y = r` — the circle's own horizontal
//!   tangent height at its topmost point, independent of `θ` (`θ` still
//!   decides where the flat's own corners are, at `(±r sinθ, r)`, by
//!   carrying the tangent line up to that fixed height rather than to a
//!   point). `y = r` is the natural truncation height because it's
//!   exactly where the round arc's own tangent would already be
//!   vertical — the flat is a continuation of the circle's silhouette,
//!   not an arbitrary cut partway up the point.
//!
//! **Which way is "up".** A hole is generated in its own local frame
//! and extruded along its own local z (same convention as
//! [`super::joint::prism_z`]); the piece that hole is cut into sits on
//! the plate at some transform, so "up" for the point to face is the
//! component of world +Z perpendicular to the hole's local z, rotated
//! back into the hole's own local x/y by the inverse of that transform
//! ([`local_up`]). When the hole's own axis is itself vertical (local z
//! parallel to world Z) there's no sideways "up" at all — the hole
//! prints fine round regardless, which is exactly when [`axis_tilt`]
//! reports 90° and [`HoleProfile::auto`] answers `Round`.

use glam::{DMat3, DVec3};

/// Which cross-section a hole or pocket gets, on top of its fit circle
/// (D24). `Round` is exactly [`circle`](super::joint::circle) (or the
/// equivalent in `split::pegs`); `Teardrop`/`FlatTop` only ever add
/// outline outside that circle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoleProfile {
    Round,
    Teardrop,
    FlatTop,
}

impl HoleProfile {
    /// Decodes a placement's recorded `profile` param (0 round, 1
    /// teardrop, 2 flat-top): an out-of-range code reads as Round, the
    /// same "never refuse on a stray numeric param" convention
    /// `Shape::from_code` (`super::joint`) already uses.
    pub fn from_code(code: f64) -> HoleProfile {
        match code.round() as i64 {
            1 => HoleProfile::Teardrop,
            2 => HoleProfile::FlatTop,
            _ => HoleProfile::Round,
        }
    }

    /// The numeric code this profile records.
    pub fn code(self) -> f64 {
        match self {
            HoleProfile::Round => 0.0,
            HoleProfile::Teardrop => 1.0,
            HoleProfile::FlatTop => 2.0,
        }
    }

    /// The automatic default for a hole axis this far from horizontal,
    /// `tilt_deg` in `0..=90` (0 = perfectly horizontal, 90 = perfectly
    /// vertical): `Teardrop` within 45° of horizontal (the hole is more
    /// sideways than vertical, so printing it round would need support
    /// or sag at its unsupported top), `Round` otherwise. This is the
    /// *suggestion*; a person's own explicit choice is kept instead of
    /// being silently overwritten (see the module docs' "re-evaluate
    /// whenever the transform changes" design note, carried by whatever
    /// records "this was auto-chosen vs. overridden" at the call site —
    /// this function itself is pure and stateless, so that bit lives in
    /// the placement's own params, not here).
    pub fn auto(tilt_deg: f64) -> HoleProfile {
        if tilt_deg.abs() <= 45.0 {
            HoleProfile::Teardrop
        } else {
            HoleProfile::Round
        }
    }
}

/// How far a hole's own local z axis (its extrusion direction, the
/// convention every `prism_z`-built hole already uses) is from vertical
/// in the world, given the piece's own rotation (`local_to_world`, the
/// upper 3×3 of its placement — translation doesn't affect a direction).
/// `0°` is a perfectly horizontal hole axis (the hole bores sideways
/// through the piece: printing it round would bridge/sag at the top);
/// `90°` is perfectly vertical (prints fine round, no bridging). Feeds
/// [`HoleProfile::auto`]; re-run this whenever the piece's transform on
/// the plate changes, so the suggestion stays current (see the module
/// docs).
pub fn axis_tilt(local_to_world: DMat3, local_z: DVec3) -> f64 {
    let world_axis = (local_to_world * local_z).normalize_or(DVec3::Z);
    // `along_z` is how much of the (unit) axis points along world Z:
    // 0 for a horizontal axis, 1 for a vertical one (either straight up
    // or straight down — it's still a vertical hole either way, so the
    // sign doesn't matter). `asin` of that is exactly the angle from
    // horizontal: 0° when along_z = 0, 90° when along_z = 1.
    let along_z = world_axis.dot(DVec3::Z).abs().clamp(0.0, 1.0);
    along_z.asin().to_degrees()
}

/// The direction the teardrop's point should face, in the hole's own
/// local x/y (for [`hole_outline`]'s `up`): the component of world +Z
/// perpendicular to the hole's own local z axis, rotated back into
/// local x/y by the inverse of `local_to_world`. `None` if the hole's
/// axis is itself vertical (world +Z has no component across the
/// cross-section at all — there's no "up" within the hole's own circle,
/// which is exactly the case [`axis_tilt`] reports as 90°, where the
/// hole prints fine round and the point's direction is moot).
pub fn local_up(local_to_world: DMat3, local_x: DVec3, local_y: DVec3, local_z: DVec3) -> Option<[f64; 2]> {
    let world_z = (local_to_world * local_z).normalize_or(DVec3::Z);
    let world_up = (DVec3::Z - world_z * world_z.dot(DVec3::Z)).try_normalize()?;
    let to_local = local_to_world.inverse() * world_up;
    let x = to_local.dot(local_x);
    let y = to_local.dot(local_y);
    let len = (x * x + y * y).sqrt();
    (len > 1e-9).then_some([x / len, y / len])
}

/// The circle, teardrop or flat-top outline of radius `r` (the fit
/// circle's own radius, untouched — see the module docs), point (or
/// flat) facing unit direction `up` (2D, in the profile's own x/y:
/// typically the component of world +Z perpendicular to the hole's
/// extrusion axis, projected into this cross-section and normalised),
/// overhang angle `overhang_deg` from vertical (clamped to a sane
/// `1..=89` so the construction never degenerates to a zero-width point
/// or a tangent that never meets the axis), counter-clockwise, with the
/// round part's own chordal tolerance `tol` (as [`super::joint::circle`]).
/// `Round` ignores `up` and `overhang_deg` and is exactly the circle.
pub fn hole_outline(
    profile: HoleProfile,
    r: f64,
    up: [f64; 2],
    overhang_deg: f64,
    tol: f64,
) -> Vec<[f64; 2]> {
    let base = super::joint::circle(r, tol);
    if profile == HoleProfile::Round || r <= 0.0 {
        return base;
    }
    let up = {
        let len = (up[0] * up[0] + up[1] * up[1]).sqrt();
        if len > 1e-9 {
            [up[0] / len, up[1] / len]
        } else {
            [0.0, 1.0]
        }
    };
    let right = [up[1], -up[0]]; // 90° clockwise from `up`: the local "+x".
    let to_local = |p: [f64; 2]| [p[0] * right[0] + p[1] * right[1], p[0] * up[0] + p[1] * up[1]];
    let from_local = |x: f64, y: f64| {
        [
            right[0] * x + up[0] * y,
            right[1] * x + up[1] * y,
        ]
    };
    let theta = overhang_deg.clamp(1.0, 89.0).to_radians();
    let (sin_t, cos_t) = theta.sin_cos();
    // The tangent points, in local (right, up) coordinates.
    let (tx, ty) = (r * sin_t, r * cos_t);
    // Keep every base point outside the wedge the straight sides
    // replace (within `theta` either side of `up`), in counter-clockwise
    // order starting just after the left tangent point, so the list ends
    // just before the right one; then the new top runs from the right
    // tangent point to the left one, closing the outline. `beta` is each
    // point's counter-clockwise angle past the left tangent direction
    // (`90° + theta` from local +x): the wedge is `beta` in
    // `[360° - 2·theta, 360°]` (and 0), the kept arc everything between.
    use std::f64::consts::{FRAC_PI_2, TAU};
    let left = FRAC_PI_2 + theta;
    let wedge = TAU - 2.0 * theta;
    let eps = 1e-9;
    let mut kept: Vec<(f64, [f64; 2])> = base
        .into_iter()
        .filter_map(|p| {
            let l = to_local(p);
            let beta = (l[1].atan2(l[0]) - left).rem_euclid(TAU);
            (beta > eps && beta < wedge - eps).then_some((beta, p))
        })
        .collect();
    kept.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out: Vec<[f64; 2]> = kept.into_iter().map(|(_, p)| p).collect();
    match profile {
        HoleProfile::Teardrop => out.extend([
            from_local(tx, ty),
            from_local(0.0, r / cos_t),
            from_local(-tx, ty),
        ]),
        // The flat sits at the circle's own horizontal-tangent height
        // (y = r, independent of theta — see the module docs): the
        // tangent line from (tx, ty), direction (-cos θ, sin θ), meets
        // y = r at parameter t = (r - ty) / sin θ, landing at
        // x = tx - t·cos θ = tx - (r - ty)·cos θ / sin θ (narrower than
        // tx, since the line leans inward as it climbs).
        HoleProfile::FlatTop => {
            let fx = tx - (r - ty) * cos_t / sin_t;
            out.extend([
                from_local(tx, ty),
                from_local(fx, r),
                from_local(-fx, r),
                from_local(-tx, ty),
            ]);
        }
        HoleProfile::Round => unreachable!("returned above"),
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // tests may panic on failure
mod tests {
    use super::*;

    const TOL: f64 = 1e-4;

    fn reach(outline: &[[f64; 2]]) -> f64 {
        outline
            .iter()
            .map(|p| (p[0] * p[0] + p[1] * p[1]).sqrt())
            .fold(0.0, f64::max)
    }

    #[test]
    fn round_is_exactly_the_circle() {
        let r = 3.0;
        let round = hole_outline(HoleProfile::Round, r, [0.0, 1.0], 45.0, TOL);
        let circle = super::super::joint::circle(r, TOL);
        assert_eq!(round, circle);
    }

    #[test]
    fn the_apex_sits_at_r_over_cos_theta() {
        let r = 4.0;
        for theta in [15.0, 30.0, 45.0, 60.0, 75.0] {
            let outline = hole_outline(HoleProfile::Teardrop, r, [0.0, 1.0], theta, TOL);
            let apex_y = reach(&outline).max(
                outline
                    .iter()
                    .map(|p| p[1])
                    .fold(f64::NEG_INFINITY, f64::max),
            );
            let want = r / theta.to_radians().cos();
            assert!(
                (apex_y - want).abs() < 1e-6,
                "theta={theta}: apex {apex_y}, want {want}"
            );
        }
    }

    #[test]
    fn forty_five_degrees_is_r_times_sqrt_2() {
        let r = 5.0;
        let outline = hole_outline(HoleProfile::Teardrop, r, [0.0, 1.0], 45.0, TOL);
        let apex_y = outline.iter().map(|p| p[1]).fold(f64::NEG_INFINITY, f64::max);
        assert!((apex_y - r * std::f64::consts::SQRT_2).abs() < 1e-6, "{apex_y}");
    }

    #[test]
    fn flat_top_truncates_at_the_circles_own_radius_regardless_of_theta() {
        let r = 6.0;
        for theta in [10.0, 45.0, 80.0] {
            let outline = hole_outline(HoleProfile::FlatTop, r, [0.0, 1.0], theta, TOL);
            let top = outline.iter().map(|p| p[1]).fold(f64::NEG_INFINITY, f64::max);
            assert!((top - r).abs() < 1e-6, "theta={theta}: top {top}");
        }
    }

    #[test]
    fn the_round_region_keeps_exactly_the_fit_radius() {
        // Every vertex at or below the tangent height (the circle's own
        // untouched arc) is still exactly on the circle of radius r:
        // teardrop/flat-top only ever add material above that height
        // (the point or the flat), never shrink or distort the round
        // region a dowel or screw actually bears against.
        let r = 7.0;
        let theta = 45.0_f64;
        let tangent_y = r * theta.to_radians().cos();
        for profile in [HoleProfile::Teardrop, HoleProfile::FlatTop] {
            let outline = hole_outline(profile, r, [0.0, 1.0], theta, TOL);
            for p in outline.iter().filter(|p| p[1] <= tangent_y + 1e-6) {
                let d = (p[0] * p[0] + p[1] * p[1]).sqrt();
                assert!(
                    (d - r).abs() < 1e-6,
                    "{profile:?}: an arc vertex at distance {d} isn't on the {r} mm fit circle"
                );
            }
            // And the outline does reach past the circle somewhere
            // (the point or the flat is really there, not vacuously
            // absent).
            let max_reach = outline
                .iter()
                .map(|p| (p[0] * p[0] + p[1] * p[1]).sqrt())
                .fold(0.0, f64::max);
            assert!(
                max_reach > r + 1e-6,
                "{profile:?}: nothing reaches past the circle ({max_reach} vs {r})"
            );
        }
    }

    #[test]
    fn the_point_faces_whatever_up_direction_is_given() {
        // Point "up" at +x instead of +y: the apex should be the
        // outline's furthest point, in the +x direction.
        let r = 3.0;
        let outline = hole_outline(HoleProfile::Teardrop, r, [1.0, 0.0], 45.0, TOL);
        let apex = outline
            .iter()
            .max_by(|a, b| a[0].total_cmp(&b[0]))
            .unwrap();
        assert!(apex[0] > r, "apex.x = {}", apex[0]);
        assert!(apex[1].abs() < 1e-6, "apex should be on the x axis: {apex:?}");
    }

    /// The outline is a simple, convex, counter-clockwise polygon for
    /// any `up` and angle (so it can be extruded by a fan), its point
    /// facing `up`.
    #[test]
    fn the_outline_is_convex_and_counter_clockwise_for_any_up() {
        for profile in [HoleProfile::Teardrop, HoleProfile::FlatTop] {
            for k in 0..16 {
                let a = std::f64::consts::TAU * k as f64 / 16.0 + 0.1;
                let up = [a.cos(), a.sin()];
                for theta in [20.0, 45.0, 70.0] {
                    let o = hole_outline(profile, 3.0, up, theta, 1e-3);
                    let n = o.len();
                    for i in 0..n {
                        let (p, q, s) = (o[i], o[(i + 1) % n], o[(i + 2) % n]);
                        let cross = (q[0] - p[0]) * (s[1] - q[1]) - (q[1] - p[1]) * (s[0] - q[0]);
                        assert!(cross > -1e-9, "{profile:?} up {up:?} θ {theta}: turn {i} is {cross}");
                    }
                    let far = o
                        .iter()
                        .max_by(|x, y| {
                            (x[0] * up[0] + x[1] * up[1]).total_cmp(&(y[0] * up[0] + y[1] * up[1]))
                        })
                        .unwrap();
                    assert!(far[0] * up[0] + far[1] * up[1] > 3.0 - 1e-9);
                }
            }
        }
    }

    #[test]
    fn auto_picks_teardrop_within_45_degrees_of_horizontal_and_round_otherwise() {
        assert_eq!(HoleProfile::auto(0.0), HoleProfile::Teardrop, "horizontal");
        assert_eq!(HoleProfile::auto(44.9), HoleProfile::Teardrop);
        assert_eq!(HoleProfile::auto(45.0), HoleProfile::Teardrop, "the boundary itself");
        assert_eq!(HoleProfile::auto(45.1), HoleProfile::Round);
        assert_eq!(HoleProfile::auto(90.0), HoleProfile::Round, "vertical");
    }

    #[test]
    fn profile_code_round_trips() {
        for (code, p) in [
            (0.0, HoleProfile::Round),
            (1.0, HoleProfile::Teardrop),
            (2.0, HoleProfile::FlatTop),
        ] {
            assert_eq!(HoleProfile::from_code(code), p);
            assert_eq!(p.code(), code);
        }
        assert_eq!(HoleProfile::from_code(99.0), HoleProfile::Round);
    }

    // -----------------------------------------------------------------
    // axis_tilt / local_up: real transforms, including one that changes
    // (the piece rotated on the plate), so Round vs Teardrop flips.
    // -----------------------------------------------------------------

    #[test]
    fn a_hole_bored_through_an_upright_pieces_side_is_horizontal() {
        // No rotation at all: a hole whose own local z is local X (the
        // usual "bore sideways across a vertical cut face" case — a
        // dowel hole across a joint whose cut face is vertical) is
        // dead horizontal in the world too.
        let identity = DMat3::IDENTITY;
        let tilt = axis_tilt(identity, DVec3::X);
        assert!((tilt - 0.0).abs() < 1e-6, "{tilt}");
        assert_eq!(HoleProfile::auto(tilt), HoleProfile::Teardrop);
    }

    #[test]
    fn a_hole_bored_straight_down_through_the_top_is_vertical() {
        let identity = DMat3::IDENTITY;
        let tilt = axis_tilt(identity, DVec3::Z);
        assert!((tilt - 90.0).abs() < 1e-6, "{tilt}");
        assert_eq!(HoleProfile::auto(tilt), HoleProfile::Round);
    }

    #[test]
    fn rotating_the_piece_on_the_plate_flips_round_to_teardrop() {
        // The same hole, local z = local X, in a piece that's flat on
        // the plate (axis horizontal, as above) versus the same piece
        // rotated 90° about Y so that local X now points straight up
        // (world Z): the hole's own local axis never changed, only the
        // piece's transform did — exactly the "re-evaluate whenever the
        // transform changes" case the module docs describe.
        let flat = DMat3::IDENTITY;
        let tilt_flat = axis_tilt(flat, DVec3::X);
        assert_eq!(HoleProfile::auto(tilt_flat), HoleProfile::Teardrop);

        let rotated = DMat3::from_axis_angle(DVec3::Y, -std::f64::consts::FRAC_PI_2);
        // local X now maps to world Z.
        assert!((rotated * DVec3::X - DVec3::Z).length() < 1e-9);
        let tilt_rotated = axis_tilt(rotated, DVec3::X);
        assert!((tilt_rotated - 90.0).abs() < 1e-6, "{tilt_rotated}");
        assert_eq!(HoleProfile::auto(tilt_rotated), HoleProfile::Round);
    }

    #[test]
    fn a_45_degree_tilt_sits_exactly_on_the_auto_boundary() {
        // A hole axis tilted 45° off horizontal: right on the
        // documented boundary, where auto still picks Teardrop ("within
        // 45°" is inclusive).
        let tilted = DMat3::from_axis_angle(DVec3::Y, -std::f64::consts::FRAC_PI_4);
        let tilt = axis_tilt(tilted, DVec3::X);
        assert!((tilt - 45.0).abs() < 1e-6, "{tilt}");
        assert_eq!(HoleProfile::auto(tilt), HoleProfile::Teardrop);
    }

    #[test]
    fn local_up_points_towards_world_z_projected_into_the_cross_section() {
        // A horizontal hole along local X, piece upright (identity):
        // the cross-section is the local Y/Z plane, and world +Z is
        // already entirely in that plane along local Z, so "up" in
        // (local Y, local Z) order is straight along local Z.
        let identity = DMat3::IDENTITY;
        let up = local_up(identity, DVec3::Y, DVec3::Z, DVec3::X).unwrap();
        assert!((up[0]).abs() < 1e-9 && (up[1] - 1.0).abs() < 1e-9, "{up:?}");
    }

    #[test]
    fn local_up_is_none_for_a_perfectly_vertical_hole() {
        // Local z = world Z exactly: world +Z has no component across
        // the cross-section at all, so there's no sideways "up".
        let identity = DMat3::IDENTITY;
        assert!(local_up(identity, DVec3::X, DVec3::Y, DVec3::Z).is_none());
    }
}
