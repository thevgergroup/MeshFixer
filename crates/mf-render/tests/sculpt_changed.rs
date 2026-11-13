#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Sculpt ▸ "Show sculpt changes": a face flagged with a
//! `FaceStates::SCULPT_CHANGED_*` tier draws visibly tinted, and a higher
//! tier draws more strongly than a lower one.

use glam::DMat4;
use mf_fixtures::grid;
use mf_render::{
    DrawItem, FaceStates, GpuMesh, MeshRenderer, MeshVertices, OrbitCamera, Palette, RenderError,
    RenderTarget, offscreen,
};

fn gpu_or_skip<T>(test: &str, r: Result<T, RenderError>) -> Option<T> {
    match r {
        Err(RenderError::NoAdapter) => {
            mf_fixtures::skip::gpu_unavailable(test);
            None
        }
        other => Some(other.unwrap()),
    }
}

fn render(tier: u8) -> Result<image::RgbaImage, RenderError> {
    let (device, queue) = offscreen::shared_device()?;
    let target = RenderTarget::new(&device, 100, 100, 4)?;
    let mut renderer = MeshRenderer::new(&device, RenderTarget::format(4), Palette::HEADLESS);
    let mesh = grid(6);
    let gpu = GpuMesh::from_vertices(&device, &MeshVertices::build(&mesh));
    if tier != 0 {
        let mut flagged: Vec<(u32, u8)> = mesh.faces().map(|(f, _)| (f.0, tier)).collect();
        let states = FaceStates::from_flagged(&mesh, &mut flagged);
        gpu.set_face_states(&queue, &states);
    }
    let cam = OrbitCamera::fit(&mesh.bounding_box().unwrap());
    let items = [DrawItem {
        hole: false,
        ghost: false,
        mesh: &gpu,
        model: DMat4::IDENTITY,
        inactive: false,
        shown_states: u32::from(FaceStates::ALL),
        object_color: None,
    }];
    target.draw(
        &device,
        &queue,
        &mut renderer,
        Some(&cam),
        &items,
        false,
        &[],
    );
    offscreen::read_back(&device, &queue, &target)
}

#[test]
fn a_flagged_tier_tints_the_surface_and_higher_tiers_tint_more() {
    let Some(none) = gpu_or_skip(
        "a_flagged_tier_tints_the_surface_and_higher_tiers_tint_more",
        render(0),
    ) else {
        return;
    };
    let low = render(FaceStates::SCULPT_CHANGED_LOW).unwrap();
    let med = render(FaceStates::SCULPT_CHANGED_MED).unwrap();
    let high = render(FaceStates::SCULPT_CHANGED_HIGH).unwrap();
    if let Some(dir) = std::env::var_os("MF_ARTIFACTS") {
        for (name, img) in [
            ("none", &none),
            ("low", &low),
            ("med", &med),
            ("high", &high),
        ] {
            img.save(std::path::Path::new(&dir).join(format!("sculpt-changed-{name}.png")))
                .unwrap();
        }
    }
    let (w, h) = (none.width(), none.height());
    let (cx, cy) = (w / 2, h / 2);
    let dist = |img: &image::RgbaImage| {
        let a = img.get_pixel(cx, cy).0;
        let b = none.get_pixel(cx, cy).0;
        (0..3)
            .map(|c| (a[c] as f64 - b[c] as f64).abs())
            .sum::<f64>()
    };
    let (d_low, d_med, d_high) = (dist(&low), dist(&med), dist(&high));
    assert!(
        d_low > 1.0,
        "the low tier should tint visibly: diff {d_low}"
    );
    assert!(
        d_med > d_low,
        "the medium tier should tint more than the low one: {d_med} vs {d_low}"
    );
    assert!(
        d_high > d_med,
        "the high tier should tint more than the medium one: {d_high} vs {d_med}"
    );
}
