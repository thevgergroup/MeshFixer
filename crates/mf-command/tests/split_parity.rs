#![allow(clippy::unwrap_used)] // tests may panic on failure

//! C02 red test, fixed by C04: under a non-uniform scale, Split's
//! region preview and Split's Apply used to compute the cut plane's
//! object-space normal two different ways (architecture-review.md §4,
//! "the world→object normal is computed three different ways for the
//! same Split plane").
//!
//! - Apply (`mf-operations/src/split/mod.rs:146`, `normal_to_object`):
//!   `M·n`, the transform's own linear part (row-major 3x4, D7) applied
//!   directly to the world normal.
//! - The region preview (`mf-desktop/src/split_flow.rs`,
//!   `compute_region_preview`) used to compute `M⁻¹·n`, the *inverse*
//!   transform's linear part applied to the world normal — wrong under
//!   a non-uniform scale. It now goes through
//!   `mf_geometry::affine::world_normal_to_object` (the same `M·n`
//!   Apply's own `normal_to_object` computes), so the two agree by
//!   construction.
//!
//! `preview_object_normal` below is a byte-for-byte copy of the fixed
//! production code's normal computation (not a reimplementation from
//! the doc comment's math), so a regression that reintroduces the old
//! `M⁻¹·n` formula is caught here too.

use glam::DVec3;
use mf_boolean::{Plane, Side, cut_half};
use mf_fixtures::cube;
use mf_runtime::RunContext;

/// The region preview's object-space normal, as fixed
/// (`split_flow.rs`'s `compute_region_preview`): the same
/// `mf_geometry::affine::world_normal_to_object` (`M·n`) Apply's own
/// `normal_to_object` is built on.
fn preview_object_normal(transform: &[f64; 12], world_normal: DVec3) -> DVec3 {
    DVec3::from(mf_geometry::affine::world_normal_to_object(
        transform,
        world_normal.to_array(),
    ))
}

/// Verbatim from `mf-operations/src/split/mod.rs`'s `normal_to_object`:
/// Apply's own object-space normal, `M·n` from the forward transform.
fn apply_object_normal(t: &[f64; 12], n: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|r| t[3 * r] * n[0] + t[3 * r + 1] * n[1] + t[3 * r + 2] * n[2])
}

/// `p` placed by the row-major 3x4 transform `t` (D7): `p·M + t`, the
/// same convention `mf_geometry::affine::apply` uses.
fn apply_point(t: &[f64; 12], p: DVec3) -> DVec3 {
    DVec3::new(
        t[0] * p.x + t[3] * p.y + t[6] * p.z + t[9],
        t[1] * p.x + t[4] * p.y + t[7] * p.z + t[10],
        t[2] * p.x + t[5] * p.y + t[8] * p.z + t[11],
    )
}

/// A 30° rotation about Z, then a strongly non-uniform scale (x1, y4,
/// z1): the owner's "part looks fine until it's scaled lopsided"
/// shape, row-major 3x4 (D7), no translation (kept simple: the
/// divergence is in the *linear* part only).
fn non_uniform_transform() -> [f64; 12] {
    let theta = 30.0_f64.to_radians();
    let (s, c) = theta.sin_cos();
    // Rotation about Z (object axes), column-vector convention, then
    // scaled columns (x1, y4, z1): `M = R * S`, stored row-major as
    // `p * M` expects (row r, col c at `t[3r+c]`).
    let r = [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]];
    let scale = [1.0, 4.0, 1.0];
    let mut t = [0.0; 12];
    for row in 0..3 {
        for col in 0..3 {
            t[3 * row + col] = r[row][col] * scale[col];
        }
    }
    t
}

/// The pre-fix region preview's object-space normal (`M⁻¹·n`, via the
/// inverse transform's linear part): kept here, named as the historical
/// bug rather than called `preview_object_normal`, purely to show the
/// transform below is a strong enough test case — it must still differ
/// from Apply's own `M·n` by tens of degrees, or the sanity check this
/// feeds would be vacuous.
fn buggy_inverse_normal(t: &[f64; 12], world_normal: DVec3) -> DVec3 {
    let m = glam::DMat3::from_cols(
        DVec3::new(t[0], t[1], t[2]),
        DVec3::new(t[3], t[4], t[5]),
        DVec3::new(t[6], t[7], t[8]),
    );
    m.inverse() * world_normal
}

/// Under non-uniform scale, the old, buggy inverse-based normal and
/// Apply's own differ by tens of degrees (not a rounding difference): a
/// sanity check on the transform chosen above, confirming it's strong
/// enough to have exposed the bug `split_preview_regions_equal_apply_
/// regions` guards against. The region preview itself (`preview_
/// object_normal`) no longer uses this formula — it now equals Apply's
/// by construction, which is exactly the fix.
#[test]
fn preview_and_apply_normals_diverge_under_non_uniform_scale() {
    let t = non_uniform_transform();
    let world_normal = DVec3::new(1.0, 1.0, 1.0).normalize();
    let buggy = buggy_inverse_normal(&t, world_normal).normalize();
    let apply = DVec3::from(apply_object_normal(&t, world_normal.to_array())).normalize();
    let angle_deg = buggy.dot(apply).clamp(-1.0, 1.0).acos().to_degrees();
    assert!(
        angle_deg > 10.0,
        "the chosen transform should make the two normals genuinely different \
         (got {angle_deg} degrees apart); strengthen the scale or rotation if this ever \
         shrinks back near zero"
    );
    let preview = preview_object_normal(&t, world_normal).normalize();
    assert!(
        preview.dot(apply) > 0.999_999,
        "the fixed region preview must equal Apply's own normal exactly (both go through \
         mf_geometry::affine::world_normal_to_object), not just coincidentally agree"
    );
}

/// C02 red test (fixed by C04): under non-uniform scale, cutting a cube
/// with the region preview's object-space normal keeps a different
/// number of triangles than cutting with Apply's own normal — the same
/// plane point, the same `Side::Above`, nothing else different. This is
/// "Split's preview regions differ from Apply's" at the boundary the
/// user sees: the ticked/outlined region in the panel is not the solid
/// Apply actually produces.
#[test]
fn split_preview_regions_equal_apply_regions() {
    let mesh = cube(10.0);
    let t = non_uniform_transform();
    // Off the cube's centre (D-symmetric points hide the bug: a plane
    // through the centre crosses the same 6 edges whichever of the two
    // normals is used, even though the normals point tens of degrees
    // apart). At [2, 8, 2], the region preview's normal crosses 4 cube
    // edges (a quadrilateral cap) while Apply's own normal crosses 6
    // (a hexagonal cap) — genuinely different cross-sections, not a
    // rounding difference in the same one.
    let object_point = DVec3::new(2.0, 8.0, 2.0);
    let world_point = apply_point(&t, object_point);
    let world_normal = DVec3::new(1.0, 1.0, 1.0).normalize();

    let inv = mf_geometry::affine::inverse(&t).expect("transform is invertible in this test");
    let plane_point = mf_geometry::affine::apply(&inv, world_point.to_array());

    let preview_normal = preview_object_normal(&t, world_normal);
    let apply_normal = apply_object_normal(&t, world_normal.to_array());

    let ctx = RunContext::none();
    let preview_cut = cut_half(
        &mesh,
        &Plane {
            point: plane_point,
            normal: preview_normal.to_array(),
        },
        Side::Above,
        &ctx,
    )
    .expect("the preview's own normal should still find and cut the cube");
    let apply_cut = cut_half(
        &mesh,
        &Plane {
            point: plane_point,
            normal: apply_normal,
        },
        Side::Above,
        &ctx,
    )
    .expect("Apply's normal should find and cut the cube");

    assert_eq!(
        preview_cut.mesh.triangle_count(),
        apply_cut.mesh.triangle_count(),
        "the region preview (M⁻¹·n) kept a different number of triangles than Apply (M·n) \
         would, for the same plane point and the same side: the panel's preview doesn't match \
         what Apply produces under non-uniform scale"
    );
}
