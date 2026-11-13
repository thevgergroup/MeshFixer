#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Checkpoint A (M4 Task 2): the real app at 1440×900, driven through two
//! transformed objects, orbit / pan / zoom, active-object highlighting, a
//! selection with its contextual Properties panel, and the history
//! breadcrumb. Every step is asserted, so this always runs as a test; with
//! `MF_ARTIFACTS=<dir>` it also writes the frames, stills and an ffmpeg
//! concat list to `<dir>/checkpoint-a/` (scripts/capture-checkpoint-a.sh
//! turns them into the MP4).

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mf_desktop::MeshFixerApp;
use mf_desktop::defects::{Defect, defect_copy};
use mf_io::threemf::{Model, ModelObject, Unit};
use mf_selection::SelectionKind;

const WINDOW: (f32, f32) = (1440.0, 900.0);
/// Frame rate of the recording (each input step is one frame).
const FPS: f64 = 15.0;

struct Recorder {
    dir: Option<PathBuf>,
    frames: usize,
    list: String,
}

impl Recorder {
    fn new() -> Recorder {
        let dir = std::env::var_os("MF_ARTIFACTS").map(|d| {
            let dir = PathBuf::from(d).join("checkpoint-a");
            std::fs::create_dir_all(dir.join("frames")).unwrap();
            dir
        });
        Recorder {
            dir,
            frames: 0,
            list: String::new(),
        }
    }

    /// Record the current frame, shown for `seconds`.
    fn shot(&mut self, h: &mut Harness<'_, MeshFixerApp>, seconds: f64) {
        let Some(dir) = &self.dir else { return };
        self.frames += 1;
        let name = format!("frame-{:04}.png", self.frames);
        h.render()
            .unwrap()
            .save(dir.join("frames").join(&name))
            .unwrap();
        writeln!(self.list, "file 'frames/{name}'\nduration {seconds:.4}").unwrap();
    }

    /// Record the current frame for `seconds` and also keep it as a still.
    fn still(&mut self, h: &mut Harness<'_, MeshFixerApp>, name: &str, seconds: f64) {
        self.shot(h, seconds);
        let Some(dir) = &self.dir else { return };
        let frame = dir
            .join("frames")
            .join(format!("frame-{:04}.png", self.frames));
        std::fs::copy(frame, dir.join(format!("{name}.png"))).unwrap();
    }

    fn finish(self) {
        let Some(dir) = &self.dir else { return };
        // The concat demuxer needs the last file repeated to honour its duration.
        let last = format!("file 'frames/frame-{:04}.png'\n", self.frames);
        std::fs::write(dir.join("frames.txt"), self.list + &last).unwrap();
    }
}

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

fn click_at(h: &mut Harness<'_, MeshFixerApp>, pos: egui::Pos2) {
    h.event(egui::Event::PointerMoved(pos));
    h.event(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    h.event(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.run();
}

fn click_label(h: &mut Harness<'_, MeshFixerApp>, label: &str) {
    let pos = h.get_by_label(label).rect().center();
    click_at(h, pos);
}

fn hover(h: &mut Harness<'_, MeshFixerApp>, pos: egui::Pos2) {
    h.event(egui::Event::PointerMoved(pos));
    h.run();
}

fn trackpad(h: &mut Harness<'_, MeshFixerApp>, delta: egui::Vec2, modifiers: egui::Modifiers) {
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta,
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Trackpad,
        modifiers,
    });
    h.run();
}

fn wheel(h: &mut Harness<'_, MeshFixerApp>, lines: f32) {
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta: egui::vec2(0.0, lines),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
        modifiers: egui::Modifiers::NONE,
    });
    h.run();
}

fn key(h: &mut Harness<'_, MeshFixerApp>, key: egui::Key) {
    for pressed in [true, false] {
        h.event(egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
    }
    h.run();
}

/// The demo scene written as a 3MF.
fn scene(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("checkpoint-a.3mf");
    let model = Model {
        metadata: Default::default(),
        unit: Unit::Millimeter,
        objects: mf_fixtures::demo_scene()
            .into_iter()
            .map(|(name, mesh, transform)| ModelObject {
                metadata: Default::default(),
                name,
                mesh,
                transform,
            })
            .collect(),
    };
    mf_io::write_model_path(&model, &path).unwrap();
    path
}

/// Where a world point of an object (object coordinates) is on screen.
fn on_screen(h: &Harness<'_, MeshFixerApp>, object: usize, local: [f64; 3]) -> egui::Pos2 {
    let o = &h.state().session.document().objects()[object];
    let world =
        mf_render::model_matrix(&o.transform) * glam::DVec4::new(local[0], local[1], local[2], 1.0);
    h.state()
        .screen_point(world.truncate().to_array())
        .expect("the point projects on screen")
}

/// After F, the target is framed: the centre of its box lands in the middle
/// of the view, and (for a target with an outward side) the camera looks at
/// it from outside, not from inside the model.
fn assert_framed(h: &Harness<'_, MeshFixerApp>, vertices: &[u32], outward_from: Option<[f64; 3]>) {
    let app = h.state();
    let object = app.session.document().object(app.active.unwrap()).unwrap();
    let mesh = object.mesh().unwrap();
    let model = mf_render::model_matrix(&object.transform);
    let mut lo = glam::DVec3::splat(f64::INFINITY);
    let mut hi = glam::DVec3::splat(f64::NEG_INFINITY);
    for &v in vertices {
        let p = glam::DVec3::from(mesh.corner_position(v));
        lo = lo.min(p);
        hi = hi.max(p);
    }
    let centre = (model * ((lo + hi) * 0.5).extend(1.0)).truncate();
    let view = app.viewport.last_rect.unwrap();
    let on_screen = app.screen_point(centre.to_array()).unwrap();
    assert!(
        (on_screen - view.center()).length() < view.height() * 0.02,
        "the target is centred: {on_screen:?} vs {:?}",
        view.center()
    );
    if let Some(inside) = outward_from {
        let inside = (model * glam::DVec3::from(inside).extend(1.0)).truncate();
        let cam = app.camera.unwrap();
        assert!(
            (centre - inside).dot(cam.eye() - cam.target) > 0.0,
            "the camera looks at the target from outside"
        );
    }
}

#[test]
fn checkpoint_a_walkthrough() {
    if !mf_render::offscreen::gpu_available() {
        mf_fixtures::skip::gpu_unavailable("checkpoint_a_walkthrough");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let path = scene(tmp.path());
    let mut h = Harness::builder()
        .with_size(WINDOW)
        // New rows and panels take a few passes to settle their styling.
        .with_max_steps(16)
        .wgpu()
        .build_eframe(move |cc| MeshFixerApp::new(cc, Some(path.clone())).unwrap());
    wait_until(&mut h, "import + analysis of both objects", |a| {
        a.session.document().objects().len() == 2
            && a.current_analysis().is_some()
            && a.object_views().len() == 2
    });
    h.run();
    let mut rec = Recorder::new();
    let step = 1.0 / FPS;

    // 1. Two transformed objects; the first imported ("scan") is active,
    //    the other is drawn desaturated.
    assert_eq!(
        h.state().object_views().iter().filter(|v| v.active).count(),
        1
    );
    rec.still(&mut h, "01-two-transformed-objects", 2.0);

    // 2. Orbit with a two-finger drag, pan with Shift, zoom with the wheel.
    let view = h.state().viewport.last_rect.unwrap();
    hover(&mut h, view.center());
    let start = h.state().camera.unwrap();
    for _ in 0..40 {
        trackpad(&mut h, egui::vec2(6.0, 1.0), egui::Modifiers::NONE);
        rec.shot(&mut h, step);
    }
    assert_ne!(h.state().camera.unwrap().yaw, start.yaw);
    rec.still(&mut h, "02-orbited", 0.8);
    for _ in 0..25 {
        trackpad(&mut h, egui::vec2(-5.0, 2.0), egui::Modifiers::SHIFT);
        rec.shot(&mut h, step);
    }
    rec.still(&mut h, "03-panned", 0.8);
    let off = view.center() + egui::vec2(-view.width() * 0.2, 0.0);
    hover(&mut h, off);
    for _ in 0..15 {
        wheel(&mut h, 1.0);
        rec.shot(&mut h, step);
    }
    rec.still(&mut h, "04-zoomed-to-cursor", 0.8);
    for _ in 0..15 {
        wheel(&mut h, -1.0);
        rec.shot(&mut h, step);
    }
    key(&mut h, egui::Key::Home);
    rec.still(&mut h, "05-home", 1.5);

    // 3. Active-object highlighting: click the plate in the view; it
    //    becomes active (full colour), the scan dims.
    let plate = h.state().session.document().objects()[1].id;
    let pos = on_screen(&h, 1, [10.0, 30.0, 0.0]);
    click_at(&mut h, pos);
    wait_until(&mut h, "plate active and analysed", |a| {
        a.active == Some(plate) && a.current_analysis().is_some()
    });
    hover(&mut h, view.right_top() + egui::vec2(-4.0, 4.0));
    rec.still(&mut h, "06-plate-active", 2.0);

    // Its defects: a crack and a tangled edge.
    let defects = h.state().current_analysis().unwrap().defects.clone();
    assert_eq!(defects.cracks.len(), 1, "{defects:?}");
    assert_eq!(defects.tangled.len(), 1, "{defects:?}");
    let row = |d: &Defect| defect_copy(d, Unit::Millimeter).row;
    click_label(&mut h, &row(&defects.cracks[0]));
    rec.still(&mut h, "07-crack-properties", 2.0);
    click_label(&mut h, &row(&defects.tangled[0]));
    h.get_by_label("Select its faces");
    rec.still(&mut h, "08-tangled-edge-properties", 2.0);
    click_label(&mut h, "Select its faces");
    wait_until(&mut h, "faces selected", |a| {
        a.selection
            .as_ref()
            .is_some_and(|s| s.kind == SelectionKind::Faces)
    });
    rec.still(&mut h, "09-tangled-faces-selected", 2.0);

    // 4. Back to the scan from the tree; hover a hole, then select it.
    click_label(&mut h, "scan");
    let scan = h.state().session.document().objects()[0].id;
    wait_until(&mut h, "scan active and analysed", |a| {
        a.active == Some(scan) && a.current_analysis().is_some()
    });
    let defects = h.state().current_analysis().unwrap().defects.clone();
    let hole = defects
        .holes
        .iter()
        .find(|d| matches!(d, Defect::Hole { .. }))
        .expect("the scan has a fillable hole");
    let row = row(hole);
    let row_pos = h.get_by_label(&row).rect().center();
    hover(&mut h, row_pos);
    wait_until(&mut h, "hover outline", |a| a.hover.is_some());
    rec.still(&mut h, "10-hover-hole-row", 1.5);
    click_at(&mut h, row_pos);
    assert_eq!(
        h.state().selection.as_ref().map(|s| s.kind),
        Some(SelectionKind::BoundaryLoop)
    );
    // The inspector card names the operation; it opens in Repair.
    click_label(&mut h, "Fill hole");
    h.run();
    rec.still(&mut h, "11-hole-selected-properties", 2.0);
    hover(&mut h, view.right_top() + egui::vec2(-4.0, 4.0));
    let before_f = h.state().camera.unwrap();
    key(&mut h, egui::Key::F);
    wait_until(&mut h, "framed on the hole", |a| a.camera != Some(before_f));
    let hole_vertices = {
        let app = h.state();
        let id = app.selection.as_ref().unwrap().ids[0];
        let topo = app.active_mesh().unwrap().topology();
        topo.boundary_loops
            .iter()
            .find(|l| l.id == id)
            .unwrap()
            .vertices
            .clone()
    };
    // The scan is a sphere about its local origin: outside is away from it.
    assert_framed(&h, &hole_vertices, Some([0.0, 0.0, 0.0]));
    rec.still(&mut h, "12-framed-selection", 2.5);

    // 5. Fill it: the breadcrumb gains the step, the patch is tinted green.
    let before = h.state().session.current();
    click_label(&mut h, "Apply");
    wait_until(&mut h, "fill + analysis", |a| {
        a.session.current() != before && a.current_analysis().is_some()
    });
    assert!(h.state().error.is_none(), "{:?}", h.state().error);
    h.run();
    rec.still(&mut h, "13-hole-filled-breadcrumb", 2.5);

    // 6. A loose piece: select it in the tree, remove it.
    // (The category is open: it has pieces.)
    h.get_by_label("Loose pieces (3)");
    let first = h.state().current_analysis().unwrap().defects.loose[0].clone();
    let piece = h
        .get_all_by_label(&defect_copy(&first, Unit::Millimeter).row)
        .next()
        .unwrap()
        .rect()
        .center();
    click_at(&mut h, piece);
    // In Repair already: the piece shows with its operation.
    h.run();
    h.get_by_label("Remove piece");
    let before_f = h.state().camera.unwrap();
    key(&mut h, egui::Key::F);
    wait_until(&mut h, "framed on the piece", |a| {
        a.camera != Some(before_f)
    });
    let piece_vertices: Vec<u32> = {
        let app = h.state();
        let id = app.selection.as_ref().unwrap().ids[0];
        let mesh = app.active_mesh().unwrap();
        mesh.topology().components[id as usize]
            .iter()
            .flat_map(|&f| mesh.face(mf_geometry::FaceId(f)).unwrap())
            .collect()
    };
    assert_framed(&h, &piece_vertices, None);
    hover(&mut h, view.right_top() + egui::vec2(-4.0, 4.0));
    rec.still(&mut h, "14-loose-piece-properties", 2.5);
    let before = h.state().session.current();
    click_label(&mut h, "Apply");
    wait_until(&mut h, "remove + analysis", |a| {
        a.session.current() != before && a.current_analysis().is_some()
    });
    key(&mut h, egui::Key::Home);
    rec.still(&mut h, "15-piece-removed", 2.0);

    // 7. Jump back through history (the History tab, in the right dock),
    // then forward.
    click_label(&mut h, "History");
    let crumb = |h: &Harness<'_, MeshFixerApp>, label: &str| {
        let view = h.state().viewport.last_rect.unwrap();
        h.get_all_by_label(label)
            .map(|n| n.rect())
            .find(|r| r.left() >= view.right() - 1.0)
            .unwrap()
            .center()
    };
    let pos = crumb(&h, "Import plate");
    click_at(&mut h, pos);
    wait_until(&mut h, "jumped back", |a| a.current_analysis().is_some());
    rec.still(&mut h, "16-breadcrumb-jumped-back", 2.0);
    let pos = crumb(&h, "Remove piece");
    click_at(&mut h, pos);
    wait_until(&mut h, "jumped forward", |a| a.current_analysis().is_some());
    rec.still(&mut h, "17-breadcrumb-forward", 2.0);
    rec.finish();
}
