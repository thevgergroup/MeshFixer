#![allow(clippy::unwrap_used)] // tests may panic on failure

//! `DrawItem::object_color` (View ▸ Colour objects, UI plan §11): an
//! object's own palette shade replaces `Palette::surface` for its front
//! faces; `None` draws the one neutral surface colour, unchanged from
//! before this field existed.

use glam::DMat4;
use image::RgbaImage;
use mf_fixtures::cube;
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

fn render(color: Option<[f32; 4]>) -> Result<RgbaImage, RenderError> {
    let (device, queue) = offscreen::shared_device()?;
    let target = RenderTarget::new(&device, 100, 100, 4)?;
    let mut renderer = MeshRenderer::new(&device, RenderTarget::format(4), Palette::HEADLESS);
    let mesh = cube(10.0);
    let gpu = GpuMesh::from_vertices(&device, &MeshVertices::build(&mesh));
    let cam = OrbitCamera::fit(&mesh.bounding_box().unwrap());
    let items = [DrawItem {
        hole: false,
        ghost: false,
        mesh: &gpu,
        model: DMat4::IDENTITY,
        inactive: false,
        shown_states: u32::from(FaceStates::ALL),
        object_color: color,
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
fn object_color_replaces_the_surface_colour() {
    let Some(plain) = gpu_or_skip("object_color_replaces_the_surface_colour", render(None)) else {
        return;
    };
    // A saturated colour, nothing like `Palette::HEADLESS.surface`'s grey.
    let tint = [1.0, 0.0, 0.0, 1.0];
    let tinted = render(Some(tint)).unwrap();
    if let Some(dir) = std::env::var_os("MF_ARTIFACTS") {
        plain
            .save(std::path::Path::new(&dir).join("object-color-off.png"))
            .unwrap();
        tinted
            .save(std::path::Path::new(&dir).join("object-color-on.png"))
            .unwrap();
    }
    let (w, h) = (plain.width(), plain.height());
    let centre = plain.get_pixel(w / 2, h / 2).0;
    let centre_tinted = tinted.get_pixel(w / 2, h / 2).0;
    assert_ne!(
        centre, centre_tinted,
        "an object colour must change the drawn surface"
    );
    // The tinted pixel reads much redder than green/blue (lit, so not
    // pure red, but clearly dominated by it), unlike the plain grey cube.
    assert!(
        centre_tinted[0] > centre_tinted[1] + 20 && centre_tinted[0] > centre_tinted[2] + 20,
        "expected a red-dominated pixel, got {centre_tinted:?}"
    );
}

#[test]
fn no_object_color_draws_the_plain_surface_unchanged() {
    let Some(plain) = gpu_or_skip(
        "no_object_color_draws_the_plain_surface_unchanged",
        render(None),
    ) else {
        return;
    };
    // The same scene through `offscreen::render_objects` (no object_color
    // field at all in its own DrawItem construction): must match exactly,
    // so `object_color: None` is provably a no-op.
    let mesh = cube(10.0);
    let cam = OrbitCamera::fit(&mesh.bounding_box().unwrap());
    let reference =
        offscreen::render_objects(&[(&mesh, DMat4::IDENTITY)], &cam, 100, 100, false).unwrap();
    assert_eq!(plain.as_raw(), reference.as_raw());
}
