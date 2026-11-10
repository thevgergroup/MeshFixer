#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Offscreen render tests for the transform gizmo and the build plate
//! (MF-OBJ-001, Task 4): the gizmo draws last with no depth test, as
//! screen-space quads like the existing 3 pt overlay lines; the plate is a
//! world-space grid at z = 0, depth-tested like the model's own geometry,
//! fading with distance. Colours come from plain values (no `mf-desktop`
//! dependency), matching [`mf_render::GizmoPalette::HEADLESS`] and
//! [`mf_render::Palette::HEADLESS`]'s plate.
//!
//! These drive `MeshRenderer`/`RenderTarget` directly (as `bench.rs` does),
//! since `draw_gizmo`/`draw_plate` need the render pass itself, not just
//! `offscreen::render_image`'s higher-level helpers.

use glam::{DMat4, DVec3};
use mf_fixtures::cube;
use mf_render::{
    DrawItem, FaceStates, Gizmo, GizmoMode, GizmoPalette, GpuMesh, Handle, MeshRenderer,
    OrbitCamera, Palette, Plate, RenderError, RenderTarget, offscreen,
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

#[test]
fn gizmo_draws_over_geometry() {
    let Some((device, queue, target, mut renderer)) =
        gpu_or_skip("gizmo_draws_over_geometry", setup(128, 128))
    else {
        return;
    };

    // A cube filling most of the view, so the gizmo's centre handle (drawn
    // at the view centre) sits squarely over its surface.
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

    // The X axis handle's tip: behind the cube from this view angle (the
    // gizmo sits at the cube's centre and the handle points away from the
    // camera on screen), so without the no-depth-test draw it would be
    // hidden by the cube's own nearer surface already in the depth buffer.
    let axis = shapes
        .iter()
        .find(|s| s.handle == Handle::Axis(0))
        .expect("Move mode has an X axis handle");
    let midpoint = ((axis.a.0 + axis.b.0) * 0.5, (axis.a.1 + axis.b.1) * 0.5);
    let (px, py) = (
        (midpoint.0.round() as u32).min(127),
        (midpoint.1.round() as u32).min(127),
    );
    let pixel = img.get_pixel(px, py).0;
    let axis_x_u8 = GizmoPalette::HEADLESS
        .axis_x
        .map(|c| (c * 255.0).round() as u8);
    let close = |a: u8, b: u8| (i32::from(a) - i32::from(b)).abs() <= 2;
    assert!(
        close(pixel[0], axis_x_u8[0])
            && close(pixel[1], axis_x_u8[1])
            && close(pixel[2], axis_x_u8[2]),
        "the gizmo's X handle must draw over the cube's surface at {px},{py}: got {pixel:?}, expected ~{axis_x_u8:?}"
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
    };
    // Framed on the plate's own extent (bigger than the cube), from close
    // to straight above, so the cube's footprint and the plate around it
    // both land clearly inside the 160x160 frame.
    let cam = top_down_camera(DVec3::new(0.0, 0.0, 0.0), 220.0, 128.0);
    let view = (160.0, 160.0);

    renderer.prepare(&queue, &cam, (160, 160));
    render_pass_frame(&device, &queue, &target, |pass| {
        renderer.draw_plate(&device, &queue, pass, &plate);
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

    let cube_px = img.get_pixel(
        hidden_under_cube.0.round() as u32,
        hidden_under_cube.1.round() as u32,
    );
    let plate_px = img.get_pixel(
        visible_on_plate.0.round() as u32,
        visible_on_plate.1.round() as u32,
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
    // A point 70% along the handle's arm: inside both the plain and the
    // (twice as wide) highlighted quad, so the same pixel compares the two.
    let sample = (
        axis.a.0 + (axis.b.0 - axis.a.0) * 0.7,
        axis.a.1 + (axis.b.1 - axis.a.1) * 0.7,
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

// --- Pure CPU-side helpers (overlay.rs): no GPU needed, always run. ---

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
            grid_mm: 10.0
        })
        .is_empty()
    );
    assert!(
        mf_render::plate_vertices(&Plate {
            size_mm: [256.0, 256.0],
            grid_mm: 0.0
        })
        .is_empty()
    );
    assert!(
        mf_render::plate_vertices(&Plate {
            size_mm: [256.0, 256.0],
            grid_mm: -1.0
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

#[test]
fn gizmo_vertices_highlighted_handle_is_lighter_and_wider() {
    let shapes = [
        mf_render::HandleShape {
            handle: Handle::Axis(0),
            a: (10.0, 10.0),
            b: (50.0, 10.0),
            radius: None,
        },
        mf_render::HandleShape {
            handle: Handle::Axis(1),
            a: (10.0, 10.0),
            b: (10.0, 50.0),
            radius: None,
        },
    ];
    let palette = GizmoPalette::HEADLESS;
    let plain = mf_render::gizmo_vertices(&shapes, None, &palette, 2.0);
    let lit = mf_render::gizmo_vertices(&shapes, Some(Handle::Axis(0)), &palette, 2.0);
    assert_eq!(plain.len(), lit.len());
    // The X handle's vertices (the first two): lighter and twice as wide.
    for i in 0..2 {
        assert!(lit[i].width_px > plain[i].width_px);
        assert_eq!(lit[i].width_px, plain[i].width_px * 2.0);
        assert!(lit[i].color[0] >= plain[i].color[0]);
        assert!(lit[i].color[0] > plain[i].color[0] || plain[i].color[0] >= 1.0);
    }
    // The untouched Y handle (vertices 2, 3) is unchanged.
    for i in 2..4 {
        assert_eq!(lit[i].color, plain[i].color);
        assert_eq!(lit[i].width_px, plain[i].width_px);
    }
}

#[test]
fn gizmo_vertices_ring_is_a_closed_polyline() {
    let shapes = [mf_render::HandleShape {
        handle: Handle::Ring(2),
        a: (50.0, 50.0),
        b: (50.0, 50.0),
        radius: Some(20.0),
    }];
    let v = mf_render::gizmo_vertices(&shapes, None, &GizmoPalette::HEADLESS, 2.0);
    assert!(v.len() >= 2);
    assert!(v.len().is_multiple_of(2));
    // Every vertex sits at the given radius from the centre.
    for vtx in &v {
        let dx = f64::from(vtx.position[0]) - 50.0;
        let dy = f64::from(vtx.position[1]) - 50.0;
        assert!(((dx * dx + dy * dy).sqrt() - 20.0).abs() < 0.5);
    }
}
