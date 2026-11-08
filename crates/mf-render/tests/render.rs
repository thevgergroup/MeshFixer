#![allow(clippy::unwrap_used)] // tests may panic on failure

use glam::{DVec3, DVec4};
use mf_fixtures::{cube, cube_with_missing_face, translated};
use mf_geometry::Mesh;
use mf_render::{OrbitCamera, RenderError, boundary_line_vertices, offscreen, surface_vertices};

#[test]
fn camera_fit_contains_bbox() {
    let m = translated(&cube(10.0), [1000.0, -50.0, 3.0]);
    let bb = m.bounding_box().unwrap();
    let cam = OrbitCamera::fit(&bb);
    let origin = DVec3::from(bb.center());
    let vp = cam.view_proj(1.5, origin).as_dmat4();
    for i in 0..8 {
        let c = DVec3::new(
            if i & 1 == 0 { bb.min[0] } else { bb.max[0] },
            if i & 2 == 0 { bb.min[1] } else { bb.max[1] },
            if i & 4 == 0 { bb.min[2] } else { bb.max[2] },
        ) - origin;
        let clip = vp * DVec4::new(c.x, c.y, c.z, 1.0);
        let ndc = clip.truncate() / clip.w;
        assert!(clip.w > 0.0);
        assert!(
            ndc.x.abs() <= 1.0 && ndc.y.abs() <= 1.0,
            "corner {i} outside: {ndc:?}"
        );
        assert!((0.0..=1.0).contains(&ndc.z), "corner {i} depth {}", ndc.z);
    }
}

#[test]
fn camera_controls_change_view() {
    let bb = cube(1.0).bounding_box().unwrap();
    let mut cam = OrbitCamera::fit(&bb);
    let before = cam.view_proj(1.0, DVec3::ZERO);
    cam.orbit(0.3, 0.1);
    assert_ne!(before, cam.view_proj(1.0, DVec3::ZERO));
    let d = cam.distance;
    cam.zoom(0.5);
    assert!((cam.distance - d * 0.5).abs() < 1e-12);
    cam.orbit(0.0, 100.0);
    assert!(cam.pitch < std::f64::consts::FRAC_PI_2);
}

#[test]
fn surface_vertices_are_local_and_flat() {
    let m = translated(&cube(1.0), [1.0e6, 0.0, 0.0]);
    let origin = DVec3::from(m.bounding_box().unwrap().center());
    let v = surface_vertices(&m, origin);
    assert_eq!(v.len(), 36);
    assert!(
        v.iter()
            .all(|x| x.position.iter().all(|c| c.abs() <= 0.5 + 1e-6))
    );
    // Flat shading: the three corners of each triangle share a unit normal.
    for tri in v.chunks(3) {
        assert_eq!(tri[0].normal, tri[1].normal);
        let n = tri[0].normal;
        assert!(((n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt() - 1.0).abs() < 1e-5);
    }
}

#[test]
fn boundary_lines_for_hole() {
    let m = cube_with_missing_face();
    assert_eq!(boundary_line_vertices(&m, DVec3::ZERO).len(), 8);
    assert!(boundary_line_vertices(&cube(1.0), DVec3::ZERO).is_empty());
}

fn gpu_or_skip<T>(test: &str, r: Result<T, RenderError>) -> Option<T> {
    match r {
        Err(RenderError::NoAdapter) => {
            mf_fixtures::skip::gpu_unavailable(test);
            None
        }
        other => Some(other.unwrap()),
    }
}

#[test]
fn offscreen_cube_not_blank() {
    let Some(img) = gpu_or_skip(
        "offscreen_cube_not_blank",
        offscreen::render_image(&cube(1.0), 64, 48, true),
    ) else {
        return;
    };
    assert_eq!(img.dimensions(), (64, 48));
    let center = img.get_pixel(32, 24).0;
    let corner = img.get_pixel(0, 0).0;
    assert_ne!(
        center, corner,
        "centre pixel should be shaded mesh, not background"
    );
    assert_eq!(corner, offscreen::BACKGROUND_RGBA);
}

#[test]
fn offscreen_empty_mesh_ok() {
    let Some(img) = gpu_or_skip(
        "offscreen_empty_mesh_ok",
        offscreen::render_image(&Mesh::empty(), 16, 16, true),
    ) else {
        return;
    };
    assert!(img.pixels().all(|p| p.0 == offscreen::BACKGROUND_RGBA));
}

#[test]
fn mesh_vertices_build_matches_helpers() {
    let m = translated(&cube_with_missing_face(), [100.0, 0.0, 0.0]);
    let v = mf_render::MeshVertices::build(&m);
    let origin = DVec3::from(m.bounding_box().unwrap().center());
    assert_eq!(v.origin, origin);
    assert_eq!(v.triangle_count, 10);
    assert_eq!(v.surface, surface_vertices(&m, origin));
    assert_eq!(v.lines, boundary_line_vertices(&m, origin));
    assert_eq!(
        mf_render::MeshVertices::build(&Mesh::empty()).surface.len(),
        0
    );
}

#[test]
fn chunk_ranges_respect_limit_and_primitive_size() {
    use mf_render::chunk_ranges;
    // 10 triangles (30 vertices) of 24-byte vertices, max 7 vertices per chunk -> 6 (2 tris).
    let r = chunk_ranges(30, 24, 24 * 7, 3);
    assert_eq!(r.first(), Some(&(0..6)));
    assert_eq!(r.last().map(|x| x.end), Some(30));
    assert!(r.iter().all(|x| x.len() % 3 == 0 && x.len() <= 7));
    assert!(r.windows(2).all(|w| w[0].end == w[1].start));
    assert!(chunk_ranges(0, 24, 1024, 3).is_empty());
    assert_eq!(chunk_ranges(4, 12, 1 << 20, 2), vec![0..4]);
}

#[test]
fn chunked_upload_renders_identically() {
    let m = mf_fixtures::uv_sphere(24, 48);
    let Some(whole) = gpu_or_skip(
        "chunked_upload_renders_identically",
        offscreen::render_image_with_chunk_limit(&m, 96, 72, true, u64::MAX),
    ) else {
        return;
    };
    let chunked = offscreen::render_image_with_chunk_limit(&m, 96, 72, true, 24 * 300).unwrap();
    assert_eq!(whole, chunked);
}

#[test]
fn oversized_render_is_an_error_not_a_panic() {
    match offscreen::render_image(&cube(1.0), 20_000, 64, true) {
        Err(RenderError::NoAdapter) => {
            mf_fixtures::skip::gpu_unavailable("oversized_render_is_an_error_not_a_panic")
        }
        Err(RenderError::TooLarge { width, height, max }) => {
            assert_eq!((width, height), (20_000, 64));
            assert!(max < 20_000);
        }
        other => panic!("expected TooLarge, got {:?}", other.map(|i| i.dimensions())),
    }
}
