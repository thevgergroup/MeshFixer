#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Transform-aware rendering and picking (MF-ISSUE-042): every object draws
//! and picks at its 3MF transform, not just in object space.

use glam::{DMat4, DVec3, DVec4};
use mf_fixtures::cube;
use mf_render::{Bvh, OrbitCamera, RenderError, model_matrix, offscreen, pick_loop, ray_pick};

fn gpu_or_skip<T>(test: &str, r: Result<T, RenderError>) -> Option<T> {
    match r {
        Err(RenderError::NoAdapter) => {
            mf_fixtures::skip::gpu_unavailable(test);
            None
        }
        other => Some(other.unwrap()),
    }
}

const IDENTITY: [f64; 12] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];

#[test]
fn model_matrix_identity_is_identity() {
    assert_eq!(model_matrix(&IDENTITY), DMat4::IDENTITY);
}

#[test]
fn model_matrix_applies_translation_as_a_row_vector_times_the_matrix() {
    // 3MF order: m00 m01 m02 m10 m11 m12 m20 m21 m22 m30 m31 m32, the last
    // three the translation. `model * p` must move the point by them.
    let t: [f64; 12] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, -3.0, 5.0, 0.5];
    let m = model_matrix(&t);
    let p = m * DVec4::new(1.0, 2.0, 3.0, 1.0);
    assert_eq!(p, DVec4::new(-2.0, 7.0, 3.5, 1.0));
}

#[test]
fn model_matrix_applies_rotation_in_3mf_row_vector_convention() {
    // 90 degree rotation about Z, 3MF row-major: row0=(0,1,0) row1=(-1,0,0) row2=(0,0,1).
    // p' = p * M means (1,0,0) -> (0,1,0).
    let t: [f64; 12] = [0.0, 1.0, 0.0, -1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];
    let m = model_matrix(&t);
    let p = m * DVec4::new(1.0, 0.0, 0.0, 1.0);
    assert!(
        (p - DVec4::new(0.0, 1.0, 0.0, 1.0)).length() < 1e-12,
        "{p:?}"
    );
}

/// A two-object document with translations (-3, 0, 0) and (3, 0, 0): the
/// gap between the cubes must not be lit, and both cubes' projected
/// centres must be lit.
#[test]
fn transformed_objects_render_where_their_transform_puts_them() {
    let left = cube(1.0);
    let right = cube(1.0);
    let left_t: [f64; 12] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, -3.0, 0.0, 0.0];
    let right_t: [f64; 12] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 3.0, 0.0, 0.0];

    // Union bounds of both cubes at their transforms, by hand (cubes are
    // axis-aligned unit boxes, so translating the mesh's own bbox is exact).
    let bb = mf_geometry::Aabb {
        min: [-3.0, 0.0, 0.0],
        max: [4.0, 1.0, 1.0],
    };
    let origin = DVec3::from(bb.center());
    let cam = OrbitCamera::fit(&bb);

    let Some(img) = gpu_or_skip(
        "transformed_objects_render_where_their_transform_puts_them",
        offscreen::render_objects(
            &[
                (&left, model_matrix(&left_t)),
                (&right, model_matrix(&right_t)),
            ],
            &cam,
            320,
            240,
            false,
        ),
    ) else {
        return;
    };

    let size = (320.0, 240.0);
    // Projected centres of each cube (its own centre, (0.5,0.5,0.5), plus
    // the translation), not corners: a corner sits on a silhouette edge,
    // where MSAA/anti-aliasing can blend it towards the background colour
    // even when the cube is drawn correctly.
    let left_center = mf_render::screen_position(&cam, origin, size, [-2.5, 0.5, 0.5]).unwrap();
    let right_center = mf_render::screen_position(&cam, origin, size, [3.5, 0.5, 0.5]).unwrap();
    let gap_center = mf_render::screen_position(&cam, origin, size, [0.5, 0.5, 0.5]).unwrap();

    let px = |p: (f64, f64)| img.get_pixel(p.0 as u32, p.1 as u32).0;
    assert_ne!(
        px(left_center),
        offscreen::BACKGROUND_RGBA,
        "left cube should be drawn at its transform"
    );
    assert_ne!(
        px(right_center),
        offscreen::BACKGROUND_RGBA,
        "right cube should be drawn at its transform"
    );
    assert_eq!(
        px(gap_center),
        offscreen::BACKGROUND_RGBA,
        "the gap between the cubes must stay background"
    );
}

#[test]
fn pick_hits_a_translated_object_in_world_space() {
    let m = cube(1.0);
    let t: [f64; 12] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 10.0, 0.0, 0.0];
    let model = model_matrix(&t);
    // Ray straight down through the translated cube's top face in world space.
    let hit = ray_pick(&m, &model, [10.3, 0.6, 5.0], [0.0, 0.0, -1.0]).unwrap();
    // The distance is reported in world space: the ray starts 5 above and
    // the top face sits at world z=1, so the hit is 4 away.
    assert!((hit.1 - 4.0).abs() < 1e-9, "{hit:?}");
    assert_eq!(
        ray_pick(&m, &model, [0.3, 0.6, 5.0], [0.0, 0.0, -1.0]),
        None,
        "object space coordinates must miss the translated object"
    );
}

#[test]
fn loop_pick_on_a_rotated_object() {
    let m = mf_fixtures::cube_with_missing_face();
    // Rotate 90 degrees about Z (3MF row-vector convention).
    let t: [f64; 12] = [0.0, 1.0, 0.0, -1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];
    let model = model_matrix(&t);
    let bb = m.bounding_box().unwrap();
    let cam = OrbitCamera::fit(&bb);
    let origin = DVec3::ZERO;
    let size = (800.0, 600.0);
    let topo = m.topology();
    let hole = &topo.boundary_loops[0];
    let a = model * DVec4::from((DVec3::from(m.corner_position(hole.vertices[0])), 1.0));
    let b = model * DVec4::from((DVec3::from(m.corner_position(hole.vertices[1])), 1.0));
    let mid_world = ((a + b) * 0.5).truncate().to_array();
    let mid = mf_render::screen_position(&cam, origin, size, mid_world).unwrap();
    assert_eq!(
        pick_loop(&m, &bvh_of(&m), &model, &cam, origin, size, mid, 8.0),
        Some(hole.id),
        "the rotated loop is still found at its world-space projection"
    );
}

#[test]
fn mirrored_object_renders_and_picks() {
    // Negative-determinant transform (mirror on X): the pipeline must keep
    // mirrored faces front-facing (Cw) so shading and picking stay correct.
    let m = cube(1.0);
    let mirror: [f64; 12] = [-1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];
    let model = model_matrix(&mirror);
    assert!(model.determinant() < 0.0);

    // The cube's own bbox is x,y,z in [0, 1]; mirrored on X it lands at
    // x in [-1, 0], so fit the camera to where it actually renders.
    let world_bb = mf_geometry::Aabb {
        min: [-1.0, 0.0, 0.0],
        max: [0.0, 1.0, 1.0],
    };
    let cam = OrbitCamera::fit(&world_bb);
    let Some(img) = gpu_or_skip(
        "mirrored_object_renders_and_picks",
        offscreen::render_objects(&[(&m, model)], &cam, 64, 48, false),
    ) else {
        return;
    };
    let center = img.get_pixel(32, 24).0;
    assert_ne!(
        center,
        offscreen::BACKGROUND_RGBA,
        "a mirrored cube must still render"
    );
    // `cull_mode` is `None`, so a wrong front-face winding would still
    // render *something* at the centre pixel, just tinted as a back face
    // (shader.wgsl: base colour (0.62, 0.30, 0.30) vs front's (0.70, 0.72,
    // 0.76)). The green channel discriminates clearly either way (shading
    // is a common scalar multiplier on all three channels, so the ratio to
    // red is shade-invariant): a front face has green >= red, a
    // (wrongly shaded) back face has green well under red.
    let (r, g) = (center[0] as f32, center[1] as f32);
    assert!(
        g >= r * 0.9,
        "the mirrored cube's near face must shade as front-facing, not back-facing: {center:?}"
    );
    // Picking: ray straight down onto the (mirrored) top face, which in
    // world space sits over x in [-1, 0].
    let hit = ray_pick(&m, &model, [-0.3, 0.6, 5.0], [0.0, 0.0, -1.0]).unwrap();
    assert!((hit.1 - 4.0).abs() < 1e-9, "{hit:?}");
}

/// A flat quad in the z=0 plane (unit square, normal (0,0,1)).
fn unit_quad() -> mf_geometry::Mesh {
    mf_geometry::Mesh::new(
        vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ],
        vec![[0, 1, 2], [0, 2, 3]],
    )
    .expect("valid")
}

/// A non-uniform shear (z' = z + k*x) must tilt shaded normals by its
/// inverse-transpose, not its own linear part: a flat quad transformed by
/// the shear's model matrix must shade identically to the same quad with
/// its vertices pre-sheared by hand (whose flat-shaded normal is the
/// geometric ground truth), because both describe the same triangles in
/// world space.
#[test]
fn shading_under_shear_matches_the_inverse_transpose_normal() {
    let k = 0.6;
    // 3MF row-major: row0=(1,0,0) row1=(0,1,0) row2=(k,0,1), i.e. z' = z + k*x
    // under the row-vector convention p' = p * M.
    let shear: [f64; 12] = [1.0, 0.0, k, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];
    let model = model_matrix(&shear);

    let quad = unit_quad();
    let (positions, triangles) = quad.to_arrays();
    let world_bb = mf_geometry::Aabb {
        min: [0.0, 0.0, 0.0],
        max: [1.0, 1.0, k],
    };
    let cam = OrbitCamera::fit(&world_bb);

    // Render A: the flat quad, placed by the shear's model matrix (exercises
    // the shader's normal matrix).
    let Some(sheared_by_gpu) = gpu_or_skip(
        "shading_under_shear_matches_the_inverse_transpose_normal",
        offscreen::render_objects(&[(&quad, model)], &cam, 64, 48, false),
    ) else {
        return;
    };

    // Render B: the same triangles, pre-sheared on the CPU and drawn with
    // an identity model. Flat shading derives its normal from the triangle
    // geometry, so this is the geometric ground truth for what the
    // triangles' normal should be after the shear.
    let sheared_positions: Vec<[f64; 3]> = positions
        .iter()
        .map(|p| [p[0], p[1], p[2] + k * p[0]])
        .collect();
    let pre_sheared = mf_geometry::Mesh::new(sheared_positions, triangles).expect("valid");
    let sheared_by_hand =
        offscreen::render_objects(&[(&pre_sheared, DMat4::IDENTITY)], &cam, 64, 48, false)
            .expect("a device that rendered once renders again");

    assert_eq!(
        sheared_by_gpu, sheared_by_hand,
        "a shear tilts shaded normals by the inverse-transpose of its linear part"
    );
}

/// The BVH picking uses for `mesh`.
fn bvh_of(mesh: &mf_geometry::Mesh) -> Bvh {
    Bvh::build(mesh, &mf_runtime::RunContext::none()).unwrap()
}
