#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The Transform tool (MF-OBJ-001, Phase 1 Task 5): the gizmo drags an
//! object (one undo per drag), Esc mid-drag commits nothing, the numeric
//! fields commit with Enter, a sheared file transform keeps its rotation
//! and scale read-only, the defect overlays give way to the axis legend,
//! and Drop to plate lands the object on the plate.
//!
//! With `MF_ARTIFACTS` set, these write Checkpoint P1-A frames there:
//! `transform-move.png`, `transform-rotate.png`, `transform-scale.png`,
//! `transform-mid-drag.png`, `transform-numeric.png`,
//! `transform-drop-before.png`, `transform-drop-after.png`,
//! `transform-sheared.png` and `transform-laptop-1280x720.png`.
//!
//! The two 30M-triangle timing tests are in `scripts/run-tests.sh`'s
//! heavy list: they run on demand only.

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::{egui, egui_wgpu};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use mf_desktop::MeshFixerApp;
use mf_io::threemf::{IDENTITY, Model, ModelObject, Unit};
use mf_render::{Decomposed, FaceStates, GizmoMode, Handle, HandleGeometry, decompose};

fn wait_until(h: &mut Harness<'_, MeshFixerApp>, what: &str, cond: impl Fn(&MeshFixerApp) -> bool) {
    let start = Instant::now();
    while !cond(h.state()) {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "timed out waiting for: {what}"
        );
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    h.run_steps(3);
}

macro_rules! require_gpu {
    ($name:literal) => {
        if !mf_render::offscreen::gpu_available() {
            mf_fixtures::skip::gpu_unavailable($name);
            return;
        }
    };
}

fn save(h: &mut Harness<'_, MeshFixerApp>, name: &str) {
    if let Ok(dir) = std::env::var("MF_ARTIFACTS") {
        let frame = h.render().unwrap();
        frame.save(std::path::Path::new(&dir).join(name)).unwrap();
    }
}

/// A 3MF with one object `name` placed by `transform`.
fn model_file(
    dir: &std::path::Path,
    name: &str,
    mesh: mf_geometry::Mesh,
    transform: [f64; 12],
) -> std::path::PathBuf {
    let path = dir.join(format!("{name}.3mf"));
    let model = Model {
        unit: Unit::Millimeter,
        objects: vec![ModelObject {
            name: name.into(),
            mesh,
            transform,
        }],
    };
    mf_io::write_model_path(&model, &path).unwrap();
    path
}

fn open_size(path: std::path::PathBuf, size: (f32, f32)) -> Harness<'static, MeshFixerApp> {
    let mut h = Harness::builder()
        .with_size(size)
        .wgpu()
        .build_eframe(move |cc| MeshFixerApp::new(cc, Some(path.clone())).unwrap());
    wait_until(&mut h, "import + analysis", |a| {
        a.current_analysis().is_some() && a.viewport.last_rect.is_some()
    });
    h
}

fn open(path: std::path::PathBuf) -> Harness<'static, MeshFixerApp> {
    open_size(path, (1440.0, 900.0))
}

/// Opens Transform with T and waits until its gizmo is drawn.
fn open_transform(h: &mut Harness<'_, MeshFixerApp>) {
    h.key_press(egui::Key::T);
    wait_until(h, "the gizmo", |a| a.transform_gizmo().is_some());
}

fn transform(h: &Harness<'_, MeshFixerApp>) -> [f64; 12] {
    let app = h.state();
    app.session
        .document()
        .object(app.active.unwrap())
        .unwrap()
        .transform
}

fn steps(h: &Harness<'_, MeshFixerApp>) -> usize {
    h.state().session.history_labels().len()
}

/// Where the X arrow's shaft is (window points), its screen direction,
/// and that a press there hits it.
fn x_arrow(h: &Harness<'_, MeshFixerApp>) -> (egui::Pos2, egui::Vec2) {
    let app = h.state();
    let g = app.transform_gizmo().unwrap();
    let cam = app.camera.unwrap();
    let rect = app.viewport.last_rect.unwrap();
    let view = (rect.width() as f64, rect.height() as f64);
    let shape = g
        .handles(&cam, view)
        .into_iter()
        .find(|s| s.handle == Handle::Axis(0))
        .expect("the X arrow is drawn");
    let HandleGeometry::Arrow {
        shaft_start,
        shaft_end,
        ..
    } = shape.geometry
    else {
        panic!("an axis is an arrow");
    };
    let mid = (
        (shaft_start.0 + shaft_end.0) / 2.0,
        (shaft_start.1 + shaft_end.1) / 2.0,
    );
    assert_eq!(g.hit(&cam, view, mid), Some(Handle::Axis(0)));
    let dir = egui::vec2(
        (shaft_end.0 - shaft_start.0) as f32,
        (shaft_end.1 - shaft_start.1) as f32,
    )
    .normalized();
    (
        rect.left_top() + egui::vec2(mid.0 as f32, mid.1 as f32),
        dir,
    )
}

fn press(h: &mut Harness<'_, MeshFixerApp>, pos: egui::Pos2) {
    h.event(egui::Event::PointerMoved(pos));
    h.event(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
}

fn move_to(h: &mut Harness<'_, MeshFixerApp>, from: egui::Pos2, to: egui::Pos2) {
    for i in 1..=10 {
        h.event(egui::Event::PointerMoved(
            from + (to - from) * (i as f32 / 10.0),
        ));
        h.step();
    }
}

fn release(h: &mut Harness<'_, MeshFixerApp>, pos: egui::Pos2) {
    h.event(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
}

fn cube_file(dir: &std::path::Path) -> std::path::PathBuf {
    model_file(dir, "cube", mf_fixtures::cube(10.0), IDENTITY)
}

#[test]
fn drag_x_arrow_moves_object_and_one_undo_restores_it() {
    require_gpu!("drag_x_arrow_moves_object_and_one_undo_restores_it");
    let dir = tempfile::tempdir().unwrap();
    let mut h = open(cube_file(dir.path()));
    // Properties lists Transform first among the whole-object tools.
    assert_eq!(h.state().tools()[0].label, "Transform");
    open_transform(&mut h);
    h.run_steps(3);
    save(&mut h, "transform-move.png");
    let (before, camera, history) = (transform(&h), h.state().camera, steps(&h));
    let revision = h.state().session.current();

    let (at, dir_x) = x_arrow(&h);
    let to = at + dir_x * 100.0;
    press(&mut h, at);
    move_to(&mut h, at, to);
    // Mid-drag only the drawn placement moved: no command yet.
    assert_eq!(steps(&h), history);
    let drawn = h
        .state()
        .shown_transform(h.state().active.unwrap())
        .unwrap();
    assert!(drawn[9] > before[9], "the drag moves the object along X");
    save(&mut h, "transform-mid-drag.png");
    release(&mut h, to);
    wait_until(&mut h, "the move commits", |a| {
        a.busy.is_none() && a.session.history_labels().len() == history + 1
    });

    let after = transform(&h);
    assert!(after[9] > before[9] + 1.0, "{after:?}");
    assert_eq!(after[..9], before[..9], "only the translation changes");
    assert_eq!((after[10], after[11]), (before[10], before[11]));
    assert_eq!(h.state().camera, camera, "the drag didn't orbit the camera");
    let status = h.state().status.clone().unwrap();
    assert!(
        status.starts_with("Moved cube by ") && status.ends_with(", 0, 0 mm"),
        "{status}"
    );
    let record = h
        .state()
        .session
        .node(h.state().session.current())
        .unwrap()
        .record
        .clone()
        .unwrap();
    assert_eq!(record.op_id, "object.set_transform");

    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run_steps(3);
    assert_eq!(transform(&h), before, "one undo puts it back");
    assert_eq!(h.state().session.current(), revision);
}

#[test]
fn escape_mid_drag_commits_nothing() {
    require_gpu!("escape_mid_drag_commits_nothing");
    let dir = tempfile::tempdir().unwrap();
    let mut h = open(cube_file(dir.path()));
    open_transform(&mut h);
    h.run_steps(3);
    let (before, history) = (transform(&h), steps(&h));
    let (at, dir_x) = x_arrow(&h);
    let to = at + dir_x * 80.0;
    press(&mut h, at);
    move_to(&mut h, at, to);
    assert_ne!(
        h.state().shown_transform(h.state().active.unwrap()),
        None,
        "dragging"
    );
    h.key_press(egui::Key::Escape);
    h.step();
    assert_eq!(
        h.state().shown_transform(h.state().active.unwrap()),
        None,
        "Esc puts the start placement back"
    );
    move_to(&mut h, to, to + dir_x * 20.0);
    release(&mut h, to + dir_x * 20.0);
    h.run_steps(5);
    assert!(h.state().busy.is_none());
    assert_eq!(steps(&h), history, "nothing committed");
    assert_eq!(transform(&h), before);
    assert!(
        h.state().transform_tool().is_some(),
        "the first Esc cancels the drag, not the tool"
    );
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(
        h.state().transform_tool().is_none(),
        "the next Esc leaves it"
    );
}

/// Types `text` into the field labelled `label` and presses Enter.
fn type_into(h: &mut Harness<'_, MeshFixerApp>, label: &str, text: &str) {
    h.get_by_label(label).focus();
    h.run_steps(2);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.run_steps(1);
    h.get_by_label(label).type_text(text);
    h.run_steps(1);
    h.key_press(egui::Key::Enter);
    h.step();
}

#[test]
fn numeric_rotation_commits_one_command() {
    require_gpu!("numeric_rotation_commits_one_command");
    let dir = tempfile::tempdir().unwrap();
    let mut h = open(cube_file(dir.path()));
    open_transform(&mut h);
    h.key_press(egui::Key::E);
    h.run_steps(3);
    assert_eq!(h.state().transform_tool().unwrap().mode, GizmoMode::Rotate);
    save(&mut h, "transform-rotate.png");
    let history = steps(&h);
    let centre = h.state().active_placement().unwrap().position;
    type_into(&mut h, "Rotation Z", "90");
    wait_until(&mut h, "the rotation commits", |a| {
        a.busy.is_none() && a.session.history_labels().len() > history
    });
    assert_eq!(steps(&h), history + 1, "one command");
    match decompose(&transform(&h)) {
        Decomposed::Trs { rotate_deg_xyz, .. } => {
            assert!((rotate_deg_xyz.z - 90.0).abs() < 1e-9, "{rotate_deg_xyz:?}");
        }
        Decomposed::Sheared => panic!("a rotation isn't a shear"),
    }
    let p = h.state().active_placement().unwrap();
    assert!(
        (p.position - centre).length() < 1e-9,
        "it turns about its centre"
    );
    assert_eq!(
        h.state().status.as_deref(),
        Some("Rotated cube 90° about Z")
    );
    h.run_steps(3);
    save(&mut h, "transform-numeric.png");
    // W and R pick the other modes.
    h.key_press(egui::Key::R);
    h.run_steps(3);
    assert_eq!(h.state().transform_tool().unwrap().mode, GizmoMode::Scale);
    save(&mut h, "transform-scale.png");
    h.key_press(egui::Key::W);
    h.run_steps(1);
    assert_eq!(h.state().transform_tool().unwrap().mode, GizmoMode::Move);
}

#[test]
fn sheared_transform_disables_rotate_and_scale_fields() {
    require_gpu!("sheared_transform_disables_rotate_and_scale_fields");
    let dir = tempfile::tempdir().unwrap();
    let mut sheared = IDENTITY;
    sheared[3] = 0.5; // y picks up half of x: a slant, not a rotation or scale
    let path = model_file(dir.path(), "slanted", mf_fixtures::cube(10.0), sheared);
    let mut h = open(path);
    open_transform(&mut h);
    h.run_steps(3);
    h.get_by_label(mf_desktop::transform_tool::SHEARED_NOTE);
    for axis in ["X", "Y", "Z"] {
        for field in ["Rotation", "Scale"] {
            let label = format!("{field} {axis}");
            assert!(
                h.get_by_label(&label).accesskit_node().is_disabled(),
                "{label} is read-only"
            );
        }
        let label = format!("Position {axis}");
        assert!(
            !h.get_by_label(&label).accesskit_node().is_disabled(),
            "{label} still edits"
        );
    }
    assert!(h.get_by_label("Rotate").accesskit_node().is_disabled());
    // Only Move: the gizmo stays a move gizmo.
    h.key_press(egui::Key::E);
    h.run_steps(2);
    assert_eq!(h.state().transform_gizmo().unwrap().mode, GizmoMode::Move);
    save(&mut h, "transform-sheared.png");
    // Its move handles still work.
    let history = steps(&h);
    let (at, dir_x) = x_arrow(&h);
    press(&mut h, at);
    move_to(&mut h, at, at + dir_x * 80.0);
    release(&mut h, at + dir_x * 80.0);
    wait_until(&mut h, "the move commits", |a| {
        a.busy.is_none() && a.session.history_labels().len() == history + 1
    });
    let t = transform(&h);
    assert_eq!(t[..9], sheared[..9], "the shear is kept");
}

/// A cube with a hole and a loose piece.
fn defective_file(dir: &std::path::Path) -> std::path::PathBuf {
    let (positions, triangles) = mf_fixtures::tetrahedron().to_arrays();
    let shifted = positions.iter().map(|p| [p[0] + 3.0, p[1], p[2]]).collect();
    let far = mf_geometry::Mesh::new(shifted, triangles).unwrap();
    let mesh = mf_fixtures::merge(&[mf_fixtures::cube_with_missing_face(), far]);
    model_file(dir, "scan", mesh, IDENTITY)
}

#[test]
fn transform_tool_hides_defect_tints_and_shows_axes_in_legend() {
    require_gpu!("transform_tool_hides_defect_tints_and_shows_axes_in_legend");
    let dir = tempfile::tempdir().unwrap();
    let mut h = open(defective_file(dir.path()));
    h.run_steps(3);
    let defect_bits = u32::from(FaceStates::LOOSE_PIECE)
        | u32::from(FaceStates::NON_MANIFOLD_ADJ)
        | u32::from(FaceStates::RECENT_REPAIR);
    assert!(h.state().holes_drawn());
    assert_ne!(h.state().shown_states() & defect_bits, 0);
    h.get_by_label("Loose pieces");

    open_transform(&mut h);
    h.run_steps(3);
    let app = h.state();
    assert!(!app.holes_drawn(), "hole outlines hidden");
    assert_eq!(app.shown_states() & defect_bits, 0, "defect tints hidden");
    assert!(app.view_layers().is_empty(), "no defect lines");
    assert!(app.hole_markers().is_none(), "no hole markers");
    for axis in ["X axis", "Y axis", "Z axis"] {
        h.get_by_label(axis);
    }
    assert!(h.query_by_label("Loose pieces").is_none());
    assert!(h.query_by_label("Recent repairs").is_none());

    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    assert!(h.state().holes_drawn(), "leaving the tool shows them again");
    h.get_by_label("Loose pieces");
}

#[test]
fn drop_to_plate_button_lands_on_plate() {
    require_gpu!("drop_to_plate_button_lands_on_plate");
    let dir = tempfile::tempdir().unwrap();
    let mut raised = IDENTITY;
    raised[11] = 7.5;
    let path = model_file(dir.path(), "part", mf_fixtures::cube(10.0), raised);
    let mut h = open(path);
    // The tree row makes the object the target, and Properties offers
    // Transform first.
    // (The first "part" is its tree row; Properties' title says it too.)
    h.get_all_by_label("part").next().unwrap().click();
    h.run_steps(2);
    h.get_by_label("Transform").click();
    wait_until(&mut h, "the gizmo", |a| a.transform_gizmo().is_some());
    h.run_steps(3);
    save(&mut h, "transform-drop-before.png");
    let history = steps(&h);
    h.get_by_label("Drop to plate").click();
    wait_until(&mut h, "the drop commits", |a| {
        a.busy.is_none() && a.session.history_labels().len() > history
    });
    assert_eq!(steps(&h), history + 1);
    let t = transform(&h);
    assert_eq!(t[11], 0.0, "the cube's base rests on z = 0: {t:?}");
    assert_eq!(t[..11], raised[..11]);
    assert_eq!(
        h.state().status.as_deref(),
        Some("Dropped part onto the plate: moved 7.50 mm down")
    );
    h.run_steps(3);
    save(&mut h, "transform-drop-after.png");
}

/// Checkpoint P1-A: the tool at a laptop size, everything reachable.
#[test]
fn transform_tool_fits_a_laptop_window() {
    require_gpu!("transform_tool_fits_a_laptop_window");
    let dir = tempfile::tempdir().unwrap();
    let mut h = open_size(cube_file(dir.path()), (1280.0, 720.0));
    open_transform(&mut h);
    h.run_steps(3);
    let window = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0));
    for label in [
        "Position X",
        "Rotation Z",
        "Scale Z",
        "Drop to plate",
        "Reset",
    ] {
        let r = h.get_by_label(label).rect();
        assert!(window.contains_rect(r), "{label} at {r:?} is on screen");
    }
    save(&mut h, "transform-laptop-1280x720.png");
}

// --- Timing (heavy list, on demand: scripts/run-tests.sh) ---

const BIG: usize = 30_000_000;
const FRAME_BUDGET: Duration = Duration::from_millis(50);

fn big_model(transform: [f64; 12]) -> mf_io::ImportedModel {
    let mesh = mf_fixtures::damaged_sphere(&mf_fixtures::DamageSpec::for_triangles(BIG, 3));
    mf_io::ImportedModel {
        objects: vec![ModelObject {
            name: "big".into(),
            mesh,
            transform,
        }],
        unit: Unit::Millimeter,
        source: mf_io::SourceInfo {
            path: "big.stl".into(),
            content_hash: mf_geometry::ContentHash([0; 32]),
            format: mf_io::Format::StlBinary,
            precision: mf_io::SourcePrecision::F32,
        },
        warnings: vec![],
    }
}

/// The app with the 30M sphere open and analysed, and its render state.
fn opened_big(transform: [f64; 12]) -> (Harness<'static, MeshFixerApp>, egui_wgpu::RenderState) {
    let mut setup = egui_kittest::wgpu::default_wgpu_setup();
    if let egui_wgpu::WgpuSetup::CreateNew(create) = &mut setup {
        create.device_descriptor = Arc::new(mf_desktop::gpu::device_descriptor);
    }
    let render_state =
        egui_kittest::wgpu::create_render_state(setup, egui_wgpu::RendererOptions::PREDICTABLE);
    let mut h = Harness::builder()
        .with_size((1440.0, 900.0))
        .renderer(egui_kittest::wgpu::WgpuTestRenderer::from_render_state(
            render_state.clone(),
        ))
        .build_eframe(|cc| MeshFixerApp::with_settings(cc, None, Default::default()).unwrap());
    h.state_mut().import_model(big_model(transform));
    let start = Instant::now();
    while !(h.state().current_analysis().is_some() && h.state().viewport.frames_rendered > 0) {
        assert!(start.elapsed() < Duration::from_secs(1200), "30M analysis");
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    (h, render_state)
}

/// Paints a frame as eframe does and waits for the GPU (as progress.rs's
/// `Painter`: headless, this stands in for presenting).
fn paint_and_wait(
    h: &Harness<'_, MeshFixerApp>,
    rs: &egui_wgpu::RenderState,
    frame: &wgpu::TextureView,
) {
    let ppp = h.ctx.pixels_per_point();
    let jobs = h.ctx.tessellate(h.output().shapes.clone(), ppp);
    let screen = egui_wgpu::ScreenDescriptor {
        size_in_pixels: [1440, 900],
        pixels_per_point: ppp,
    };
    let mut renderer = rs.renderer.write();
    let mut encoder = rs.device.create_command_encoder(&Default::default());
    let uploads = renderer.update_buffers(&rs.device, &rs.queue, &mut encoder, &jobs, &screen);
    {
        let mut pass = encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("test egui"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: frame,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                ..Default::default()
            })
            .forget_lifetime();
        renderer.render(&mut pass, &jobs, &screen);
    }
    rs.queue
        .submit(uploads.into_iter().chain(std::iter::once(encoder.finish())));
    drop(renderer);
    rs.device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(Duration::from_secs(10)),
        })
        .unwrap();
}

fn frame_texture(rs: &egui_wgpu::RenderState) -> wgpu::TextureView {
    rs.device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("test frame"),
            size: wgpu::Extent3d {
                width: 1440,
                height: 900,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: rs.target_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
        .create_view(&Default::default())
}

fn p95(mut times: Vec<Duration>) -> Duration {
    times.sort();
    times[(times.len() * 95 / 100).min(times.len() - 1)]
}

/// A drag only changes a uniform (MF-OBJ-001): dragging a 30M object
/// costs no more than orbiting it, within 1 ms at the 95th percentile.
#[test]
fn dragging_a_30m_object_keeps_frames_under_orbit_plus_1ms() {
    require_gpu!("dragging_a_30m_object_keeps_frames_under_orbit_plus_1ms");
    let (mut h, rs) = opened_big(IDENTITY);
    let frame = frame_texture(&rs);
    open_transform(&mut h);
    let timed = |h: &mut Harness<'_, MeshFixerApp>,
                 step: &mut dyn FnMut(&mut Harness<'_, MeshFixerApp>)| {
        let start = Instant::now();
        step(h);
        paint_and_wait(h, &rs, &frame);
        start.elapsed()
    };
    for _ in 0..5 {
        timed(&mut h, &mut |h| {
            h.state_mut().camera.as_mut().unwrap().orbit(0.01, 0.0);
            h.step();
        });
    }
    let orbit: Vec<Duration> = (0..60)
        .map(|_| {
            timed(&mut h, &mut |h| {
                h.state_mut().camera.as_mut().unwrap().orbit(0.002, 0.0);
                h.step();
            })
        })
        .collect();
    let (at, dir_x) = x_arrow(&h);
    press(&mut h, at);
    let mut drag = Vec::new();
    for i in 1..=60 {
        let p = at + dir_x * (2.0 * i as f32);
        drag.push(timed(&mut h, &mut |h| {
            h.event(egui::Event::PointerMoved(p));
            h.step();
        }));
    }
    assert!(
        h.state()
            .shown_transform(h.state().active.unwrap())
            .is_some()
    );
    let (o, d) = (p95(orbit), p95(drag[5..].to_vec()));
    eprintln!(
        "orbit p95 {:.2} ms, drag p95 {:.2} ms",
        o.as_secs_f64() * 1e3,
        d.as_secs_f64() * 1e3
    );
    assert!(
        d <= o + Duration::from_millis(1),
        "dragging ({d:?}) costs more than orbiting ({o:?}) + 1 ms"
    );
}

/// MF-RT-001: Drop to plate on 30M runs on the worker; no UI frame goes
/// over 50 ms from the click until it has committed.
#[test]
fn ui_frames_stay_under_50ms_while_dropping_30m_to_plate() {
    require_gpu!("ui_frames_stay_under_50ms_while_dropping_30m_to_plate");
    let mut raised = IDENTITY;
    raised[11] = 50.0;
    let (mut h, rs) = opened_big(raised);
    let frame = frame_texture(&rs);
    open_transform(&mut h);
    for _ in 0..5 {
        h.step();
        paint_and_wait(&h, &rs, &frame);
    }
    let history = steps(&h);
    h.get_by_label("Drop to plate").click();
    let mut frames = Vec::new();
    let mut started = false;
    loop {
        let start = Instant::now();
        h.state_mut().camera.as_mut().unwrap().orbit(0.002, 0.0);
        h.step();
        paint_and_wait(&h, &rs, &frame);
        frames.push(start.elapsed());
        started |= h.state().busy.is_some();
        assert!(started || frames.len() < 10, "the drop didn't start");
        if started && h.state().busy.is_none() {
            break;
        }
        assert!(frames.len() < 100_000, "never finished");
    }
    assert_eq!(steps(&h), history + 1, "the drop committed");
    let worst = frames.iter().max().unwrap();
    eprintln!(
        "worst frame {:.1} ms over {} frames",
        worst.as_secs_f64() * 1e3,
        frames.len()
    );
    assert!(*worst < FRAME_BUDGET, "a frame took {worst:?}");
}
