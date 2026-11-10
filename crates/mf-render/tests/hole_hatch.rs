#![allow(clippy::unwrap_used)] // tests may panic on failure

//! A Hole object draws translucent with a grey diagonal hatch (a pattern,
//! not a colour): checked on the pixels.

use glam::DMat4;
use image::RgbaImage;
use mf_fixtures::cube;
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

fn luma(img: &RgbaImage, x: u32, y: u32) -> f64 {
    let p = img.get_pixel(x, y).0;
    0.299 * p[0] as f64 + 0.587 * p[1] as f64 + 0.114 * p[2] as f64
}

/// How often brightness crosses its mean along row `y`, x in `range`.
fn crossings(img: &RgbaImage, y: u32, range: std::ops::Range<u32>) -> usize {
    let values: Vec<f64> = range.map(|x| luma(img, x, y)).collect();
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    values
        .windows(2)
        .filter(|w| (w[0] - mean).signum() != (w[1] - mean).signum())
        .count()
}

#[test]
fn a_hole_is_translucent_and_hatched() {
    let block = cube(10.0);
    let cam = OrbitCamera::fit(&block.bounding_box().unwrap());
    let (w, h) = (200, 200);
    let render =
        |hole: bool| offscreen::render_roles(&[(&block, DMat4::IDENTITY, hole)], &cam, w, h, false);
    let Some(solid) = gpu_or_skip("a_hole_is_translucent_and_hatched", render(false)) else {
        return;
    };
    let hole = render(true).unwrap();
    if let Some(dir) = std::env::var_os("MF_ARTIFACTS") {
        hole.save(std::path::Path::new(&dir).join("hole-hatch.png"))
            .unwrap();
    }
    let empty: Vec<(&mf_geometry::Mesh, DMat4, bool)> = Vec::new();
    let background = offscreen::render_roles(&empty, &cam, w, h, false).unwrap();
    // Across the middle of the cube: the solid is smoothly shaded, the
    // hole striped (8 pt apart, so many times across 60 px).
    let (y, xs) = (h / 2, (w / 2 - 30)..(w / 2 + 30));
    let solid_x = crossings(&solid, y, xs.clone());
    let hole_x = crossings(&hole, y, xs.clone());
    assert!(solid_x <= 4, "the solid is plain: {solid_x} crossings");
    assert!(hole_x >= 8, "the hole is hatched: {hole_x} crossings");
    // Translucent: on average much nearer the background than the solid
    // is (about 35% opaque, a bit more on the stripes).
    let mean = |img: &RgbaImage| {
        xs.clone()
            .map(|x| {
                let (a, b) = (img.get_pixel(x, y).0, background.get_pixel(x, y).0);
                (0..3)
                    .map(|c| (a[c] as f64 - b[c] as f64).abs())
                    .sum::<f64>()
            })
            .sum::<f64>()
            / xs.len() as f64
    };
    let (solid_d, hole_d) = (mean(&solid), mean(&hole));
    assert!(hole_d < 0.75 * solid_d, "hole {hole_d} vs solid {solid_d}");
    assert!(hole_d > 0.0, "the hole is drawn");
}
