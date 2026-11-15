#![allow(clippy::unwrap_used)] // tests may panic on failure

//! C02 red test: under a non-uniform scale, Split's region preview and
//! Split's Apply compute the cut plane's object-space normal two
//! different ways (architecture-review.md §4, "the world→object normal
//! is computed three different ways for the same Split plane").
//!
//! - Apply (`mf-operations/src/split/mod.rs:146`, `normal_to_object`):
//!   `M·n`, the transform's own linear part (row-major 3x4, D7) applied
//!   directly to the world normal.
//! - The region preview (`mf-desktop/src/split_flow.rs:529-535`,
//!   `compute_region_preview`): `M⁻¹·n`, the *inverse* transform's
//!   linear part applied to the world normal. The comment there says
//!   this "matches a uniform-scale/rotation transform exactly and is
//!   close enough... under a non-uniform one" — this test is the "not
//!   close enough" case.
//!
//! Both helpers below are byte-for-byte copies of the production code
//! (not a reimplementation from the doc comment's math), so a fix that
//! changes the real formula is caught here too.
//!
//! Fixed by: C04 (switches the region preview to the op's own `M·n`).

use glam::DVec3;
use mf_boolean::{Plane, Side, cut_half};
use mf_fixtures::cube;
use mf_runtime::RunContext;

/// Verbatim from `mf-desktop/src/split_flow.rs`'s `linear_and_translation`.
fn linear_and_translation(t: &[f64; 12]) -> (glam::DMat3, DVec3) {
    (
        glam::DMat3::from_cols(
            DVec3::new(t[0], t[1], t[2]),
            DVec3::new(t[3], t[4], t[5]),
            DVec3::new(t[6], t[7], t[8]),
        ),
        DVec3::new(t[9], t[10], t[11]),
    )
}

/// Verbatim from `mf-desktop/src/split_flow.rs`'s `inverse_transform`.
fn inverse_transform(t: &[f64; 12]) -> Option<[f64; 12]> {
    let (l, tr) = linear_and_translation(t);
    if l.determinant().abs() < 1e-18 {
        return None;
    }
    let inv = l.inverse();
    let it = -(inv * tr);
    Some([
        inv.x_axis.x,
        inv.y_axis.x,
        inv.z_axis.x,
        inv.x_axis.y,
        inv.y_axis.y,
        inv.z_axis.y,
        inv.x_axis.z,
        inv.y_axis.z,
        inv.z_axis.z,
        it.x,
        it.y,
        it.z,
    ])
}

/// The region preview's object-space normal (`split_flow.rs:529-535`):
/// `M⁻¹·n`, via the inverse transform's linear part.
fn preview_object_normal(transform: &[f64; 12], world_normal: DVec3) -> DVec3 {
    let inv = inverse_transform(transform).expect("transform is invertible in this test");
    let (linear, _translation) = linear_and_translation(&inv);
    linear * world_normal
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

/// Under non-uniform scale, the region preview's object-space normal
/// and Apply's own differ by tens of degrees (not a rounding
/// difference): a sanity check on the transform chosen above, before
/// trusting the mesh-level assertion that follows.
#[test]
fn preview_and_apply_normals_diverge_under_non_uniform_scale() {
    let t = non_uniform_transform();
    let world_normal = DVec3::new(1.0, 1.0, 1.0).normalize();
    let preview = preview_object_normal(&t, world_normal).normalize();
    let apply = DVec3::from(apply_object_normal(&t, world_normal.to_array())).normalize();
    let angle_deg = preview.dot(apply).clamp(-1.0, 1.0).acos().to_degrees();
    assert!(
        angle_deg > 10.0,
        "the chosen transform should make the two normals genuinely different \
         (got {angle_deg} degrees apart); strengthen the scale or rotation if this ever \
         shrinks back near zero"
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
#[ignore = "red: C04"]
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
