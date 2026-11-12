#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Curvature ("cavity") shading: a dent or a bump reads apart from a flat
//! or gently curved surface, from screen-space normal derivatives alone
//! (no extra vertex data).

use glam::DMat4;
use image::RgbaImage;
use mf_fixtures::uv_sphere;
use mf_geometry::Mesh;
use mf_render::{OrbitCamera, RenderError, offscreen};

fn gpu_or_skip<T>(test: &str, r: Result<T, RenderError>) -> Option<T> {
    match r {
        Err(RenderError::NoAdapter) => {
            mf_fixtures::skip::gpu_unavailable(test);
            None
        }
        other => Some(other.unwrap()),
    }
}

/// A unit sphere with the vertices nearest `centre` pushed `amount` along
/// their own radius (negative: a dent; positive: a bump), a smooth falloff
/// within `radius` so the rest of the sphere stays a plain sphere.
fn dented_sphere(centre: [f64; 3], radius: f64, amount: f64) -> Mesh {
    let sphere = uv_sphere(24, 48);
    let c = glam::DVec3::from(centre);
    let positions: Vec<[f64; 3]> = (0..sphere.vertex_slots())
        .map(|v| {
            let p = glam::DVec3::from(sphere.position(mf_geometry::VertexId(v)).unwrap());
            let d = (p - c).length();
            let t = (1.0 - d / radius).clamp(0.0, 1.0);
            let falloff = t * t * (3.0 - 2.0 * t);
            (p * (1.0 + amount * falloff)).to_array()
        })
        .collect();
    let faces: Vec<[u32; 3]> = sphere.faces().map(|(_, t)| t).collect();
    Mesh::new(positions, faces).unwrap()
}

/// The darkest and brightest luma in a square window (screen space),
/// skipping background pixels (the sphere's own silhouette; a 100×100
/// render's background is always far darker than its lit surface, so a
/// generous floor safely excludes it without needing the exact colour).
fn luma_range(img: &RgbaImage, cx: u32, cy: u32, half: u32) -> (f64, f64) {
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for x in cx - half..cx + half {
        for y in cy - half..cy + half {
            let p = img.get_pixel(x, y).0;
            let l = 0.299 * p[0] as f64 + 0.587 * p[1] as f64 + 0.114 * p[2] as f64;
            if l < 15.0 {
                continue; // background
            }
            lo = lo.min(l);
            hi = hi.max(l);
        }
    }
    (lo, hi)
}

#[test]
fn a_dent_reads_apart_from_a_plain_sphere() {
    let plain = uv_sphere(24, 48);
    let cam = OrbitCamera::fit(&plain.bounding_box().unwrap());
    let (w, h) = (300, 300);
    let render = |mesh: &Mesh| {
        offscreen::render_objects(&[(mesh, DMat4::IDENTITY)], &cam, w, h, false)
    };
    let Some(plain_img) = gpu_or_skip("a_dent_reads_apart_from_a_plain_sphere", render(&plain))
    else {
        return;
    };
    // A dent facing the camera (+Z pole, which `OrbitCamera::fit` frames
    // near the centre of a sphere's silhouette at a typical pitch).
    let dented = dented_sphere([0.0, 0.0, 1.0], 0.6, -0.35);
    let dented_img = render(&dented).unwrap();
    if let Some(dir) = std::env::var_os("MF_ARTIFACTS") {
        plain_img
            .save(std::path::Path::new(&dir).join("cavity-plain.png"))
            .unwrap();
        dented_img
            .save(std::path::Path::new(&dir).join("cavity-dented.png"))
            .unwrap();
    }
    // A plain sphere's lit surface, over a window spanning its dome, has
    // a smooth, narrow luma range (the studio lighting's gentle
    // gradient). A dent's hollow adds a visibly darker patch at its rim
    // on top of that same gradient, widening the range well past the
    // plain sphere's.
    let (cx, cy, half) = (w / 2, h / 2 - 20, 70);
    let (plain_lo, plain_hi) = luma_range(&plain_img, cx, cy, half);
    let (dented_lo, dented_hi) = luma_range(&dented_img, cx, cy, half);
    assert!(
        dented_hi - dented_lo > (plain_hi - plain_lo) * 1.1,
        "a dent should widen the luma range over the same region of a plain sphere: \
         plain {plain_lo}..{plain_hi}, dented {dented_lo}..{dented_hi}"
    );
}

#[test]
fn a_bump_reads_apart_from_a_plain_sphere() {
    let plain = uv_sphere(24, 48);
    let cam = OrbitCamera::fit(&plain.bounding_box().unwrap());
    let (w, h) = (300, 300);
    let render = |mesh: &Mesh| {
        offscreen::render_objects(&[(mesh, DMat4::IDENTITY)], &cam, w, h, false)
    };
    let Some(plain_img) = gpu_or_skip("a_bump_reads_apart_from_a_plain_sphere", render(&plain))
    else {
        return;
    };
    let bumped = dented_sphere([0.0, 0.0, 1.0], 0.6, 0.35);
    let bumped_img = render(&bumped).unwrap();
    if let Some(dir) = std::env::var_os("MF_ARTIFACTS") {
        bumped_img
            .save(std::path::Path::new(&dir).join("cavity-bumped.png"))
            .unwrap();
    }
    let (cx, cy, half) = (w / 2, h / 2 - 20, 70);
    let (plain_lo, plain_hi) = luma_range(&plain_img, cx, cy, half);
    let (bumped_lo, bumped_hi) = luma_range(&bumped_img, cx, cy, half);
    assert!(
        bumped_hi - bumped_lo > (plain_hi - plain_lo) * 1.02,
        "a bump should widen the luma range over the same region of a plain sphere: \
         plain {plain_lo}..{plain_hi}, bumped {bumped_lo}..{bumped_hi}"
    );
}
