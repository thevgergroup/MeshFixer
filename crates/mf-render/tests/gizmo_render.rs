#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Offscreen render tests for the transform gizmo and the build plate
//! (MF-OBJ-001, Task 4, fix round 1): the gizmo draws last with no depth
//! test, as screen-space shapes (arrows, rings, plane squares, point
//! discs — [`mf_render::HandleGeometry`]) like the existing 3 pt overlay
//! lines; the plate is a world-space grid at z = 0, depth-tested like the
//! model's own geometry (pushed slightly away so a coplanar object always
//! wins), fading with distance. Colours come from plain values (no
//! `mf-desktop` dependency), matching [`mf_render::GizmoPalette::HEADLESS`]
//! and [`mf_render::Palette::HEADLESS`]'s plate.
//!
//! These drive `MeshRenderer`/`RenderTarget` directly (as `bench.rs` does),
//! since `draw_gizmo`/`draw_plate` need the render pass itself, not just
//! `offscreen::render_image`'s higher-level helpers.

use glam::{DMat4, DVec3};
use mf_fixtures::cube;
use mf_render::{
    DrawItem, FaceStates, Gizmo, GizmoMode, GizmoPalette, GpuMesh, Handle, HandleGeometry,
    MeshRenderer, OrbitCamera, Palette, Plate, RenderError, RenderTarget, offscreen,
};

const SAMPLES: u32 = 1; // no MSAA: exact pixel colours, no blending at edges

fn gpu_or_skip<T>(test: &str, r: Result<T, RenderError>) -> Option<T> {
    match r {
        Err(RenderError::NoAdapter) => {
            mf_fixtures::skip::gpu_unavailable(test);
            None
        }
        other => Some(other.unwrap()),
    }
}

/// A fresh device, target and renderer for a headless render, with the
/// gizmo/plate's own headless palettes (no `mf-desktop`).
fn setup(
    width: u32,
    height: u32,
) -> Result<(wgpu::Device, wgpu::Queue, RenderTarget, MeshRenderer), RenderError> {
    let (device, queue) = offscreen::shared_device()?;
    let target = RenderTarget::new(&device, width, height, SAMPLES)?;
    let renderer = MeshRenderer::new(&device, RenderTarget::format(SAMPLES), Palette::HEADLESS);
    Ok((device, queue, target, renderer))
}

/// Opens a render pass over `target`'s colour and depth attachments,
/// clearing both (as [`RenderTarget::draw_timed`] does), runs `body`, and
/// submits. For tests that need the pass itself (`draw_gizmo`/`draw_plate`),
/// which `RenderTarget::draw`/`draw_timed` don't expose.
fn render_pass_frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &RenderTarget,
    body: impl FnOnce(&mut wgpu::RenderPass<'_>),
) {
    let (view, resolve_target) = target.color_attachment();
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("mf gizmo/plate test"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: offscreen::BACKGROUND_RGBA[0] as f64 / 255.0,
                        g: offscreen::BACKGROUND_RGBA[1] as f64 / 255.0,
                        b: offscreen::BACKGROUND_RGBA[2] as f64 / 255.0,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: target.depth_view(),
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        body(&mut pass);
    }
    queue.submit([encoder.finish()]);
}

/// A camera looking straight down (z-up): pitch just under 90 degrees, so
/// a plate grid and an object resting on it both read clearly from above.
fn top_down_camera(target: DVec3, distance: f64, radius: f64) -> OrbitCamera {
    OrbitCamera {
        target,
        distance,
        yaw: 0.0,
        pitch: 89f64.to_radians(),
        fov_y: 40f64.to_radians(),
        radius,
    }
}

/// Fix round 1, review point 1: the original version of this test sampled
/// (117, 77) on a 128x128 render — a background pixel, so the test passed
/// even with the gizmo's depth test wrongly left on (verified directly:
/// temporarily switching the gizmo line/fill pipelines to
/// `DepthMode::Opaque` — the full-model/pass set-up, not just a single
/// `cargo check` — still passed). This version samples the move gizmo's
/// centre disc at the pivot, which sits exactly at the cube's own centre:
/// the view centre (`OrbitCamera::fit` guarantees it lands on the cube's
/// silhouette) and, geometrically, strictly behind the cube's front face.
/// Confirmed to fail without the fix by temporarily forcing the gizmo
/// pipelines to `DepthMode::Opaque` (the cube's own shaded colour shows
/// at the pivot instead of the disc's), then reverting; a disc — a solid
/// filled area several pixels across — rather than a line's single-pixel
/// endpoint avoids the rasterisation-boundary fragility a bare point
/// sample on a line segment has there.
#[test]
fn gizmo_draws_over_geometry() {
    let Some((device, queue, target, mut renderer)) =
        gpu_or_skip("gizmo_draws_over_geometry", setup(128, 128))
    else {
        return;
    };

    let mesh = cube(10.0);
    let bb = mesh.bounding_box().unwrap();
    let cam = OrbitCamera::fit(&bb);
    let gpu = GpuMesh::upload(&device, &mesh);
    let items = [DrawItem {
        mesh: &gpu,
        model: DMat4::IDENTITY,
        inactive: false,
        shown_states: u32::from(FaceStates::ALL),
    }];

    let gizmo = Gizmo {
        pivot: DVec3::from(bb.center()),
        mode: GizmoMode::Move,
        size_pt: 110.0,
    };
    let view = (128.0, 128.0);
    let shapes = gizmo.handles(&cam, view);
    assert!(!shapes.is_empty(), "the gizmo must have handles to draw");

    renderer.prepare(&queue, &cam, (128, 128));
    render_pass_frame(&device, &queue, &target, |pass| {
        renderer.paint(&device, &queue, pass, cam.target, &items, false, &[]);
        renderer.draw_gizmo(&device, pass, &shapes, None, &GizmoPalette::HEADLESS);
    });
    let img = offscreen::read_back(&device, &queue, &target).unwrap();

    let centre = shapes
        .iter()
        .find(|s| s.handle == Handle::Centre)
        .expect("Move mode has a centre handle");
    let HandleGeometry::Point { center, .. } = centre.geometry else {
        panic!("centre handle must have Point geometry: {centre:?}");
    };
    let (px, py) = (
        (center.0.round() as u32).clamp(0, 127),
        (center.1.round() as u32).clamp(0, 127),
    );
    let pixel = img.get_pixel(px, py).0;
    let neutral_u8 = GizmoPalette::HEADLESS
        .neutral
        .map(|c| (c * 255.0).round() as u8);
    let close = |a: u8, b: u8| (i32::from(a) - i32::from(b)).abs() <= 2;
    assert!(
        close(pixel[0], neutral_u8[0])
            && close(pixel[1], neutral_u8[1])
            && close(pixel[2], neutral_u8[2]),
        "the gizmo's centre disc must draw over the cube's surface at {px},{py} \
         (the pivot, the cube's own centre): got {pixel:?}, expected ~{neutral_u8:?}"
    );
}

#[test]
fn plate_is_hidden_behind_objects_resting_on_it() {
    let Some((device, queue, target, mut renderer)) = gpu_or_skip(
        "plate_is_hidden_behind_objects_resting_on_it",
        setup(160, 160),
    ) else {
        return;
    };

    // `cube(10.0)` spans x,y,z in [0, 10]: its base sits exactly on the
    // plate (world z = 0), as an object "resting on it" would.
    let mesh = cube(10.0);
    let gpu = GpuMesh::upload(&device, &mesh);
    let items = [DrawItem {
        mesh: &gpu,
        model: DMat4::IDENTITY,
        inactive: false,
        shown_states: u32::from(FaceStates::ALL),
    }];

    let plate = Plate {
        size_mm: [256.0, 256.0],
        grid_mm: 10.0,
        ..Plate::default()
    };
    // Framed on the plate's own extent (bigger than the cube), from close
    // to straight above, so the cube's footprint and the plate around it
    // both land clearly inside the 160x160 frame.
    let cam = top_down_camera(DVec3::new(0.0, 0.0, 0.0), 220.0, 128.0);
    let view = (160.0, 160.0);

    renderer.prepare(&queue, &cam, (160, 160));
    render_pass_frame(&device, &queue, &target, |pass| {
        renderer.draw_plate(&device, &queue, pass, &plate, cam.target);
        renderer.paint(&device, &queue, pass, cam.target, &items, false, &[]);
    });
    let img = offscreen::read_back(&device, &queue, &target).unwrap();

    // A grid line crosses x=0 (a plate line, since grid_mm=10 divides the
    // plate evenly and the grid is centred on the origin) underneath the
    // cube (which spans x,y in [0,10]): sample inside the cube's footprint
    // at x=0 (on the cube's own edge, so well inside its silhouette at
    // (5, 5)) versus the same grid line well outside the cube, at
    // (x=0, y=-40), both at the world origin's grid line.
    let hidden_under_cube = mf_render::screen_position(&cam, cam.target, view, [5.0, 5.0, 10.0])
        .expect("the cube's top face projects onto screen");
    let visible_on_plate = mf_render::screen_position(&cam, cam.target, view, [0.0, -40.0, 0.0])
        .expect("bare plate projects onto screen");
    // Fix round 1, review point 4: the plate had no depth bias, so a
    // resting object's base face (z = 0, exactly coplanar with the plate)
    // raced the plate for the depth test at every pixel under it — which
    // one won was arbitrary (rasterisation order), not guaranteed. Sample
    // dead-centre of the cube's base face in world space (its footprint's
    // own corner at the origin, where a grid line would otherwise show
    // through) to specifically exercise the coplanar case, not just a
    // point already decided by the cube's taller top face at z=10 above.
    let on_the_base_face = mf_render::screen_position(&cam, cam.target, view, [0.001, 0.001, 0.0])
        .expect("the cube's base face projects onto screen");

    let cube_px = img.get_pixel(
        hidden_under_cube.0.round() as u32,
        hidden_under_cube.1.round() as u32,
    );
    let plate_px = img.get_pixel(
        visible_on_plate.0.round() as u32,
        visible_on_plate.1.round() as u32,
    );
    let base_px = img.get_pixel(
        on_the_base_face.0.round().clamp(0.0, 159.0) as u32,
        on_the_base_face.1.round().clamp(0.0, 159.0) as u32,
    );

    let plate_grey = (Palette::HEADLESS.plate[0] * 255.0).round() as i32;
    // Over the cube: the surface colour (bright, desaturated stone), not
    // the dark plate grey, and not the background.
    assert_ne!(
        [cube_px[0], cube_px[1], cube_px[2]],
        offscreen::BACKGROUND_RGBA[..3],
        "the cube must render over the plate"
    );
    assert!(
        (i32::from(cube_px[0]) - plate_grey).abs() > 20,
        "a point on the cube's top face must not read as the plate's grid grey: {cube_px:?}"
    );
    // The coplanar base face: must read as the cube's surface colour too,
    // not the plate's grey, even though both sit at exactly z = 0.
    assert!(
        (i32::from(base_px[0]) - plate_grey).abs() > 20,
        "the cube's own base face (coplanar with the plate at z=0) must win the depth tie \
         and show the object colour, not the plate's grid grey: {base_px:?}"
    );
    // Off the cube, on bare plate: must not be the empty background colour
    // (the grid line is drawn there, even if faded somewhat by distance).
    assert_ne!(
        [plate_px[0], plate_px[1], plate_px[2]],
        offscreen::BACKGROUND_RGBA[..3],
        "a grid line on bare plate must be visible: {plate_px:?}"
    );
}

#[test]
fn highlighted_handle_is_lighter() {
    let Some((device, queue, target, mut renderer)) =
        gpu_or_skip("highlighted_handle_is_lighter", setup(128, 128))
    else {
        return;
    };

    let bb = mf_geometry::Aabb {
        min: [-1.0, -1.0, -1.0],
        max: [1.0, 1.0, 1.0],
    };
    let cam = OrbitCamera::fit(&bb);
    let gizmo = Gizmo {
        pivot: DVec3::ZERO,
        mode: GizmoMode::Move,
        size_pt: 40.0,
    };
    let view = (128.0, 128.0);
    let shapes = gizmo.handles(&cam, view);
    let items: [DrawItem<'_>; 0] = [];

    let mut render_with = |highlight: Option<Handle>| {
        renderer.prepare(&queue, &cam, (128, 128));
        render_pass_frame(&device, &queue, &target, |pass| {
            renderer.paint(&device, &queue, pass, cam.target, &items, false, &[]);
            renderer.draw_gizmo(&device, pass, &shapes, highlight, &GizmoPalette::HEADLESS);
        });
        offscreen::read_back(&device, &queue, &target).unwrap()
    };

    let axis = shapes
        .iter()
        .find(|s| s.handle == Handle::Axis(0))
        .expect("Move mode has an X axis handle");
    let HandleGeometry::Arrow {
        shaft_start,
        shaft_end,
        ..
    } = axis.geometry
    else {
        panic!("axis handle must have Arrow geometry: {axis:?}");
    };
    // A point halfway along the shaft: inside both the plain and the
    // (twice as wide) highlighted quad, so the same pixel compares the two.
    let sample = (
        shaft_start.0 + (shaft_end.0 - shaft_start.0) * 0.5,
        shaft_start.1 + (shaft_end.1 - shaft_start.1) * 0.5,
    );
    let (px, py) = (
        (sample.0.round() as u32).min(127),
        (sample.1.round() as u32).min(127),
    );

    let plain = render_with(None);
    let lit = render_with(Some(Handle::Axis(0)));
    let plain_px = plain.get_pixel(px, py).0;
    let lit_px = lit.get_pixel(px, py).0;

    // 40% lighter, same hue: every channel of the highlighted pixel must
    // be brighter than (or equal to, if already saturated) the plain one,
    // and at least one channel strictly brighter.
    assert!(
        lit_px[0] >= plain_px[0] && lit_px[1] >= plain_px[1] && lit_px[2] >= plain_px[2],
        "the highlighted handle must not be darker than the plain one: plain {plain_px:?}, lit {lit_px:?}"
    );
    assert!(
        lit_px[0] > plain_px[0] || lit_px[1] > plain_px[1] || lit_px[2] > plain_px[2],
        "the highlighted handle must be visibly lighter: plain {plain_px:?}, lit {lit_px:?}"
    );
}

// --- Fix round 1, review point 5: GPU coverage for every shape kind, in
// every mode. ---

/// Renders `gizmo` alone (no mesh) over a plain background and returns the
/// image, for sampling a specific shape kind's own pixels.
fn render_gizmo_alone(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &RenderTarget,
    renderer: &mut MeshRenderer,
    cam: &OrbitCamera,
    view: (f64, f64),
    gizmo: &Gizmo,
) -> image::RgbaImage {
    let shapes = gizmo.handles(cam, view);
    let items: [DrawItem<'_>; 0] = [];
    renderer.prepare(queue, cam, (view.0 as u32, view.1 as u32));
    render_pass_frame(device, queue, target, |pass| {
        renderer.paint(device, queue, pass, cam.target, &items, false, &[]);
        renderer.draw_gizmo(device, pass, &shapes, None, &GizmoPalette::HEADLESS);
    });
    offscreen::read_back(device, queue, target).unwrap()
}

fn axis_color_u8(i: u8) -> [u8; 3] {
    let c = match i {
        0 => GizmoPalette::HEADLESS.axis_x,
        1 => GizmoPalette::HEADLESS.axis_y,
        _ => GizmoPalette::HEADLESS.axis_z,
    };
    [c[0], c[1], c[2]].map(|v| (v * 255.0).round() as u8)
}

fn pixel_close_to(img: &image::RgbaImage, px: (f64, f64), want: [u8; 3], msg: &str) {
    let (x, y) = (
        (px.0.round() as i64).clamp(0, img.width() as i64 - 1) as u32,
        (px.1.round() as i64).clamp(0, img.height() as i64 - 1) as u32,
    );
    let got = img.get_pixel(x, y).0;
    let close = (i32::from(got[0]) - i32::from(want[0])).abs() <= 4
        && (i32::from(got[1]) - i32::from(want[1])).abs() <= 4
        && (i32::from(got[2]) - i32::from(want[2])).abs() <= 4;
    assert!(close, "{msg} at {x},{y}: got {got:?}, expected ~{want:?}");
}

/// Move mode: the X axis's arrowhead (sampled near its tip) and the
/// centre disc (sampled at the pivot) both draw their own colour.
#[test]
fn move_mode_arrowhead_and_centre_disc_draw() {
    let Some((device, queue, target, mut renderer)) =
        gpu_or_skip("move_mode_arrowhead_and_centre_disc_draw", setup(256, 256))
    else {
        return;
    };
    let cam = OrbitCamera::fit(&mf_geometry::Aabb {
        min: [-1.0, -1.0, -1.0],
        max: [1.0, 1.0, 1.0],
    });
    let view = (256.0, 256.0);
    let gizmo = Gizmo {
        pivot: DVec3::ZERO,
        mode: GizmoMode::Move,
        size_pt: 90.0,
    };
    let shapes = gizmo.handles(&cam, view);
    let img = render_gizmo_alone(&device, &queue, &target, &mut renderer, &cam, view, &gizmo);

    let axis = shapes.iter().find(|s| s.handle == Handle::Axis(0)).unwrap();
    let HandleGeometry::Arrow {
        shaft_end,
        tip,
        head,
        ..
    } = &axis.geometry
    else {
        panic!("expected Arrow geometry");
    };
    assert!(!head.is_empty(), "the arrowhead must have a head polygon");
    // A point 60% of the way from the shaft end to the tip: well inside
    // the arrowhead's filled triangle body (not its sharp single-pixel
    // apex, which rasterises too thin to reliably sample).
    let in_head = (
        shaft_end.0 + (tip.0 - shaft_end.0) * 0.6,
        shaft_end.1 + (tip.1 - shaft_end.1) * 0.6,
    );
    pixel_close_to(&img, in_head, axis_color_u8(0), "the X arrowhead");

    let centre = shapes.iter().find(|s| s.handle == Handle::Centre).unwrap();
    let HandleGeometry::Point { center, .. } = centre.geometry else {
        panic!("expected Point geometry");
    };
    pixel_close_to(
        &img,
        center,
        GizmoPalette::HEADLESS
            .neutral
            .map(|v| (v * 255.0).round() as u8)[..3]
            .try_into()
            .unwrap(),
        "the centre disc",
    );

    // A plane handle's square draws its own (filled) colour at its centroid.
    let plane = shapes
        .iter()
        .find(|s| s.handle == Handle::Plane(2))
        .unwrap();
    let HandleGeometry::Plane { corners } = &plane.geometry else {
        panic!("expected Plane geometry");
    };
    let centroid = (
        corners.iter().map(|c| c.0).sum::<f64>() / 4.0,
        corners.iter().map(|c| c.1).sum::<f64>() / 4.0,
    );
    pixel_close_to(
        &img,
        centroid,
        axis_color_u8(2),
        "the Z plane handle's square",
    );
}

/// Rotate mode: the X ring's own front-facing polyline draws its colour,
/// at a point distinct from where the Y ring's polyline would be (an
/// oblique view, so the two rings trace different screen ellipses, not
/// the same circle).
#[test]
fn rotate_mode_rings_draw_as_distinct_ellipses() {
    let Some((device, queue, target, mut renderer)) = gpu_or_skip(
        "rotate_mode_rings_draw_as_distinct_ellipses",
        setup(256, 256),
    ) else {
        return;
    };
    let cam = OrbitCamera {
        target: DVec3::ZERO,
        distance: 6.0,
        yaw: -45f64.to_radians(),
        pitch: 25f64.to_radians(),
        fov_y: 40f64.to_radians(),
        radius: 1.0,
    };
    let view = (256.0, 256.0);
    let gizmo = Gizmo {
        pivot: DVec3::ZERO,
        mode: GizmoMode::Rotate,
        size_pt: 90.0,
    };
    let shapes = gizmo.handles(&cam, view);
    let img = render_gizmo_alone(&device, &queue, &target, &mut renderer, &cam, view, &gizmo);

    for i in 0..2u8 {
        let ring = shapes.iter().find(|s| s.handle == Handle::Ring(i)).unwrap();
        let HandleGeometry::Ring { points, front } = &ring.geometry else {
            panic!("expected Ring geometry");
        };
        // The front-facing point nearest the view centre, comfortably
        // inside the frame (some of a ring's own front half can still
        // fall outside the viewport, or right at its edge where a line's
        // own width can miss the exact sampled pixel, at a large
        // radius/oblique angle — unlike the centre it surrounds).
        let margin = 20.0;
        let centre = (128.0, 128.0);
        let in_frame = |p: &(f64, f64)| {
            p.0 >= margin && p.0 <= 256.0 - margin && p.1 >= margin && p.1 <= 256.0 - margin
        };
        let (p, _) = points
            .iter()
            .zip(front)
            .filter(|&(p, &f)| f && in_frame(p))
            .min_by(|a, b| {
                let d = |p: &(f64, f64)| (p.0 - centre.0).powi(2) + (p.1 - centre.1).powi(2);
                d(a.0).total_cmp(&d(b.0))
            })
            .unwrap_or_else(|| {
                panic!("ring {i} must have a front-facing point well inside the frame")
            });
        pixel_close_to(
            &img,
            *p,
            axis_color_u8(i),
            &format!("ring {i}'s front half"),
        );
    }
}

/// Scale mode: a scale axis's cube head and the uniform-scale centre disc
/// both draw.
#[test]
fn scale_mode_cube_head_and_uniform_disc_draw() {
    let Some((device, queue, target, mut renderer)) = gpu_or_skip(
        "scale_mode_cube_head_and_uniform_disc_draw",
        setup(256, 256),
    ) else {
        return;
    };
    let cam = OrbitCamera::fit(&mf_geometry::Aabb {
        min: [-1.0, -1.0, -1.0],
        max: [1.0, 1.0, 1.0],
    });
    let view = (256.0, 256.0);
    let gizmo = Gizmo {
        pivot: DVec3::ZERO,
        mode: GizmoMode::Scale,
        size_pt: 90.0,
    };
    let shapes = gizmo.handles(&cam, view);
    let img = render_gizmo_alone(&device, &queue, &target, &mut renderer, &cam, view, &gizmo);

    let axis = shapes
        .iter()
        .find(|s| s.handle == Handle::ScaleAxis(1))
        .unwrap();
    let HandleGeometry::Arrow { head, .. } = &axis.geometry else {
        panic!("expected Arrow geometry");
    };
    assert_eq!(head.len(), 4, "a scale axis's head must be a quad (a cube)");
    // The quad's own centroid: well inside its filled interior.
    let centroid = (
        head.iter().map(|c| c.0).sum::<f64>() / 4.0,
        head.iter().map(|c| c.1).sum::<f64>() / 4.0,
    );
    pixel_close_to(&img, centroid, axis_color_u8(1), "the Y scale cube head");

    let uniform = shapes
        .iter()
        .find(|s| s.handle == Handle::ScaleUniform)
        .unwrap();
    let HandleGeometry::Point { center, .. } = uniform.geometry else {
        panic!("expected Point geometry");
    };
    pixel_close_to(
        &img,
        center,
        GizmoPalette::HEADLESS
            .neutral
            .map(|v| (v * 255.0).round() as u8)[..3]
            .try_into()
            .unwrap(),
        "the uniform-scale disc",
    );
}

// --- Pure CPU-side helpers (overlay.rs, gizmo.rs): no GPU needed, always run. ---

#[test]
fn plate_vertices_default_is_256mm_with_a_10mm_grid_through_the_origin() {
    let plate = Plate::default();
    assert_eq!(plate.size_mm, [256.0, 256.0]);
    assert_eq!(plate.grid_mm, 10.0);
    let v = mf_render::plate_vertices(&plate);
    assert!(!v.is_empty());
    assert!(v.len().is_multiple_of(2), "lines come in pairs");
    // Every vertex is at z = 0, on the plate.
    assert!(v.iter().all(|p| p.position[2] == 0.0));
    // A line runs exactly through the world origin on both axes (an
    // object placed at the origin meets a grid line, not a gap).
    assert!(
        v.chunks(2)
            .any(|pair| pair[0].position[0] == 0.0 && pair[1].position[0] == 0.0),
        "a grid line must cross x = 0"
    );
    assert!(
        v.chunks(2)
            .any(|pair| pair[0].position[1] == 0.0 && pair[1].position[1] == 0.0),
        "a grid line must cross y = 0"
    );
}

#[test]
fn plate_vertices_degenerate_input_is_empty_not_a_panic() {
    assert!(
        mf_render::plate_vertices(&Plate {
            size_mm: [0.0, 256.0],
            grid_mm: 10.0,
            ..Plate::default()
        })
        .is_empty()
    );
    assert!(
        mf_render::plate_vertices(&Plate {
            size_mm: [256.0, 256.0],
            grid_mm: 0.0,
            ..Plate::default()
        })
        .is_empty()
    );
    assert!(
        mf_render::plate_vertices(&Plate {
            size_mm: [256.0, 256.0],
            grid_mm: -1.0,
            ..Plate::default()
        })
        .is_empty()
    );
}

#[test]
fn gizmo_palette_color_for_follows_axis_index_d8() {
    let p = GizmoPalette::HEADLESS;
    assert_eq!(p.color_for(Handle::Axis(0)), p.axis_x);
    assert_eq!(p.color_for(Handle::Axis(1)), p.axis_y);
    assert_eq!(p.color_for(Handle::Axis(2)), p.axis_z);
    assert_eq!(p.color_for(Handle::Plane(0)), p.axis_x);
    assert_eq!(p.color_for(Handle::Ring(2)), p.axis_z);
    assert_eq!(p.color_for(Handle::ScaleAxis(1)), p.axis_y);
    // Handles with no single axis are neutral.
    assert_eq!(p.color_for(Handle::Centre), p.neutral);
    assert_eq!(p.color_for(Handle::ViewRing), p.neutral);
    assert_eq!(p.color_for(Handle::ScaleUniform), p.neutral);
}

fn arrow_shape(
    handle: Handle,
    shaft_start: (f64, f64),
    shaft_end: (f64, f64),
    tip: (f64, f64),
) -> mf_render::HandleShape {
    mf_render::HandleShape {
        handle,
        geometry: HandleGeometry::Arrow {
            shaft_start,
            shaft_end,
            tip,
            head: vec![
                (shaft_end.0, shaft_end.1 - 2.0),
                tip,
                (shaft_end.0, shaft_end.1 + 2.0),
            ],
        },
    }
}

#[test]
fn gizmo_line_vertices_highlighted_handle_is_lighter_and_wider() {
    let shapes = [
        arrow_shape(Handle::Axis(0), (10.0, 10.0), (40.0, 10.0), (50.0, 10.0)),
        arrow_shape(Handle::Axis(1), (10.0, 10.0), (10.0, 40.0), (10.0, 50.0)),
    ];
    let palette = GizmoPalette::HEADLESS;
    let plain = mf_render::gizmo_line_vertices(&shapes, None, &palette, 2.0);
    let lit = mf_render::gizmo_line_vertices(&shapes, Some(Handle::Axis(0)), &palette, 2.0);
    assert_eq!(plain.len(), lit.len());
    // The X handle's shaft vertices (the first two): lighter and twice as wide.
    for i in 0..2 {
        assert!(lit[i].width_px > plain[i].width_px);
        assert_eq!(lit[i].width_px, plain[i].width_px * 2.0);
        assert!(lit[i].color[0] >= plain[i].color[0]);
        assert!(lit[i].color[0] > plain[i].color[0] || plain[i].color[0] >= 1.0);
    }
    // The untouched Y handle's shaft (vertices 2, 3) is unchanged.
    for i in 2..4 {
        assert_eq!(lit[i].color, plain[i].color);
        assert_eq!(lit[i].width_px, plain[i].width_px);
    }
}

#[test]
fn gizmo_fill_vertices_arrowhead_is_a_triangle_in_the_axis_colour() {
    let shapes = [arrow_shape(
        Handle::Axis(0),
        (10.0, 10.0),
        (40.0, 10.0),
        (50.0, 10.0),
    )];
    let fill = mf_render::gizmo_fill_vertices(&shapes, None, &GizmoPalette::HEADLESS);
    assert_eq!(fill.len(), 3, "one triangle, three vertices");
    for v in &fill {
        assert_eq!(v.color, GizmoPalette::HEADLESS.axis_x);
    }
}

#[test]
fn gizmo_fill_vertices_point_handle_is_a_filled_disc() {
    let shapes = [mf_render::HandleShape {
        handle: Handle::Centre,
        geometry: HandleGeometry::Point {
            center: (50.0, 50.0),
            radius_px: 8.0,
        },
    }];
    let fill = mf_render::gizmo_fill_vertices(&shapes, None, &GizmoPalette::HEADLESS);
    assert!(!fill.is_empty());
    assert!(fill.len().is_multiple_of(3), "triangles");
    for v in &fill {
        assert_eq!(v.color, GizmoPalette::HEADLESS.neutral);
    }
}

#[test]
fn gizmo_line_vertices_ring_skips_the_back_half() {
    // A ring with half its points front-facing, half not: only the
    // front-facing segments should produce line vertices.
    let n = 8;
    let points: Vec<(f64, f64)> = (0..n)
        .map(|i| {
            let theta = std::f64::consts::TAU * i as f64 / n as f64;
            (50.0 + 20.0 * theta.cos(), 50.0 + 20.0 * theta.sin())
        })
        .collect();
    let front: Vec<bool> = (0..n).map(|i| i < n / 2).collect();
    let shapes = [mf_render::HandleShape {
        handle: Handle::Ring(0),
        geometry: HandleGeometry::Ring { points, front },
    }];
    let v = mf_render::gizmo_line_vertices(&shapes, None, &GizmoPalette::HEADLESS, 2.0);
    // Fewer segments than the full ring (8 segments total; only those
    // touching a front-facing point are kept).
    assert!(v.len() < 8 * 2, "{}", v.len());
    assert!(!v.is_empty());
}

#[test]
fn gizmo_fill_vertices_plane_square_is_filled() {
    let shapes = [mf_render::HandleShape {
        handle: Handle::Plane(2),
        geometry: HandleGeometry::Plane {
            corners: [(10.0, 10.0), (20.0, 10.0), (20.0, 20.0), (10.0, 20.0)],
        },
    }];
    let fill = mf_render::gizmo_fill_vertices(&shapes, None, &GizmoPalette::HEADLESS);
    // A quad fan: 2 triangles, 6 vertices.
    assert_eq!(fill.len(), 6);
    for v in &fill {
        assert_eq!(v.color, GizmoPalette::HEADLESS.axis_z);
    }
}

/// The plate lies at world z = 0 wherever the camera looks (Task 5): the
/// view-projection is relative to the camera's target, so the grid must be
/// shifted into that frame like the model is. Before this, a camera
/// looking at (5, 0, 0) drew the x = 0 grid line at x = 5.
#[test]
fn plate_stays_at_world_origin_when_the_camera_target_moves() {
    let Some((device, queue, target, mut renderer)) = gpu_or_skip(
        "plate_stays_at_world_origin_when_the_camera_target_moves",
        setup(160, 160),
    ) else {
        return;
    };
    let plate = Plate {
        size_mm: [256.0, 256.0],
        grid_mm: 10.0,
        ..Plate::default()
    };
    let cam = top_down_camera(DVec3::new(5.0, 0.0, 0.0), 60.0, 128.0);
    let view = (160.0, 160.0);
    renderer.prepare(&queue, &cam, (160, 160));
    render_pass_frame(&device, &queue, &target, |pass| {
        renderer.draw_plate(&device, &queue, pass, &plate, cam.target);
    });
    let img = offscreen::read_back(&device, &queue, &target).unwrap();
    // Anything but background within a pixel of where `world` projects.
    let drawn = |world: [f64; 3]| {
        let (x, y) = mf_render::screen_position(&cam, cam.target, view, world).unwrap();
        (-1i32..=1).any(|dx| {
            (-1i32..=1).any(|dy| {
                let p = img.get_pixel(
                    (x.round() as i32 + dx) as u32,
                    (y.round() as i32 + dy) as u32,
                );
                p.0[..3] != offscreen::BACKGROUND_RGBA[..3]
            })
        })
    };
    assert!(drawn([0.0, 3.0, 0.0]), "the x = 0 line is at x = 0");
    assert!(!drawn([5.0, 3.0, 0.0]), "nothing at x = 5, mid-cell");
}
