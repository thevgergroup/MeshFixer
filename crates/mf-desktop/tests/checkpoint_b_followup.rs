#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Checkpoint B follow-up (M4 Task 8a): the real app at 1440×900 on the
//! real Archaeopteryx scan, finding items from the side panel. Hover a
//! tiny loose piece's row and double-click it; double-click a hole on the
//! far side; double-click the object's row. Each is framed with the
//! animated camera move and the selection's pulse. Every step is asserted,
//! so this always runs as a test (recording a skip when the scan isn't
//! fetched); with `MF_ARTIFACTS=<dir>` it also writes the frames, stills
//! and an ffmpeg concat list to `<dir>/checkpoint-b-followup/`
//! (scripts/capture-checkpoint-b-followup.sh turns them into the MP4).

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use glam::DVec3;
use mf_desktop::MeshFixerApp;
use mf_desktop::defects::{Defect, defect_copy};
use mf_render::{FaceStates, model_matrix};

const WINDOW: (f32, f32) = (1440.0, 900.0);
/// Every frame of a camera move is recorded at this rate (the harness
/// steps at it too).
const FPS: f64 = 60.0;
/// How long each still is shown in the recording.
const STILL: f64 = 1.6;

/// Frames and stills, written only with `MF_ARTIFACTS` set.
struct Recorder {
    dir: Option<PathBuf>,
    frames: usize,
    list: String,
}

impl Recorder {
    fn new() -> Recorder {
        let dir = std::env::var_os("MF_ARTIFACTS").map(|d| {
            let dir = PathBuf::from(d).join("checkpoint-b-followup");
            std::fs::create_dir_all(dir.join("frames")).unwrap();
            dir
        });
        Recorder {
            dir,
            frames: 0,
            list: String::new(),
        }
    }

    fn frame_name(&self) -> String {
        format!("{:05}.png", self.frames)
    }

    fn shot(&mut self, h: &mut Harness<'_, MeshFixerApp>, seconds: f64) {
        let Some(dir) = &self.dir else { return };
        self.frames += 1;
        let name = self.frame_name();
        h.render()
            .unwrap()
            .save(dir.join("frames").join(&name))
            .unwrap();
        writeln!(self.list, "file 'frames/{name}'\nduration {seconds:.4}").unwrap();
    }

    fn still(&mut self, h: &mut Harness<'_, MeshFixerApp>, name: &str) {
        self.shot(h, STILL);
        let Some(dir) = &self.dir else { return };
        let frame = dir.join("frames").join(self.frame_name());
        std::fs::copy(frame, dir.join(format!("{name}.png"))).unwrap();
    }

    /// The concat list (the capture script repeats the last file, as the
    /// concat demuxer needs to honour its duration).
    fn finish(self) {
        let Some(dir) = &self.dir else { return };
        std::fs::write(dir.join("frames.txt"), self.list).unwrap();
    }
}

fn wait_until(
    h: &mut Harness<'_, MeshFixerApp>,
    what: &str,
    timeout: Duration,
    cond: impl Fn(&MeshFixerApp) -> bool,
) {
    let start = Instant::now();
    while !cond(h.state()) {
        assert!(start.elapsed() < timeout, "timed out waiting for: {what}");
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    h.run_steps(3);
}

const SHORT: Duration = Duration::from_secs(60);

fn press(h: &mut Harness<'_, MeshFixerApp>, pos: egui::Pos2, pressed: bool) {
    h.event(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
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
    h.step();
}

fn row_pos(h: &Harness<'_, MeshFixerApp>, label: &str) -> egui::Pos2 {
    h.get_all_by_label(label)
        .next()
        .unwrap_or_else(|| panic!("tree row {label:?}"))
        .rect()
        .center()
}

/// Rests the pointer on the row (recording the hover), then double-clicks
/// it and records every frame of the camera move and the pulse after it.
fn double_click(h: &mut Harness<'_, MeshFixerApp>, rec: &mut Recorder, label: &str) {
    let pos = row_pos(h, label);
    h.event(egui::Event::PointerMoved(pos));
    h.step();
    h.run_steps(20);
    assert_eq!(row_pos(h, label), pos, "the row stays put");
    rec.shot(h, 0.8);
    for _ in 0..2 {
        press(h, pos, true);
        press(h, pos, false);
    }
    let mut steps = 0;
    while h.state().camera_animating() {
        steps += 1;
        assert!(steps < 6000, "the camera move never ended");
        h.step();
        rec.shot(h, 1.0 / FPS);
    }
    // The pulse (600 ms).
    for _ in 0..40 {
        h.step();
        rec.shot(h, 1.0 / FPS);
    }
}

/// Where `points`' bounding box lands on screen: its centre's distance from
/// the view centre and its larger extent, as fractions of the view height.
fn on_screen(app: &MeshFixerApp, points: &[DVec3]) -> (f32, f32) {
    let view = app.viewport.last_rect.unwrap();
    let lo = points.iter().fold(DVec3::splat(f64::MAX), |a, p| a.min(*p));
    let hi = points.iter().fold(DVec3::splat(f64::MIN), |a, p| a.max(*p));
    let corners: Vec<egui::Pos2> = (0..8)
        .map(|i| {
            let pick = |k: usize| if i & (1 << k) == 0 { lo[k] } else { hi[k] };
            app.screen_point([pick(0), pick(1), pick(2)]).unwrap()
        })
        .collect();
    let rect = egui::Rect::from_points(&corners);
    let c = app.screen_point(((lo + hi) * 0.5).to_array()).unwrap();
    (
        c.distance(view.center()) / view.height(),
        rect.width().max(rect.height()) / view.height(),
    )
}

/// Every point of a hole or loose piece of the active object (world).
fn points(app: &MeshFixerApp, d: &Defect) -> Vec<DVec3> {
    let object = app.session.document().object(app.active.unwrap()).unwrap();
    let mesh = object.mesh().unwrap();
    let model = model_matrix(&object.transform);
    let topo = mesh.topology();
    let vertices: Vec<u32> = match d {
        Defect::Hole { loop_id, .. } | Defect::OverCap { loop_id, .. } => topo
            .boundary_loops
            .iter()
            .find(|l| l.id == *loop_id)
            .unwrap()
            .vertices
            .clone(),
        Defect::LoosePiece { component, .. } => topo.components[*component as usize]
            .iter()
            .flat_map(|&f| mesh.face(mf_geometry::FaceId(f)).unwrap())
            .collect(),
        _ => unreachable!("holes and pieces only"),
    };
    vertices
        .into_iter()
        .map(|v| model.transform_point3(DVec3::from(mesh.corner_position(v))))
        .collect()
}

/// Scrolls the tree (the wheel over it) until row `label` is in view, in
/// its upper part.
fn scroll_to(h: &mut Harness<'_, MeshFixerApp>, label: &str) {
    let heading = h.get_by_label("Objects & Defects").rect();
    let over = egui::pos2(heading.center().x, WINDOW.1 * 0.5);
    h.event(egui::Event::PointerMoved(over));
    h.step();
    let mut tries = 0;
    let mut last = None;
    loop {
        let at = h.query_by_label(label).map(|n| n.rect());
        if at.is_some() && at == last {
            break; // as far as it scrolls
        }
        last = at;
        let lines = match at {
            Some(r) if r.top() < heading.bottom() + 20.0 => 1.0,
            Some(r) if r.bottom() > WINDOW.1 * 0.6 => -1.0,
            Some(_) => break,
            None => -1.0,
        };
        tries += 1;
        assert!(tries < 400, "scrolled to {label:?}");
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(0.0, lines),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: egui::Modifiers::NONE,
        });
        h.run_steps(2);
    }
    // Let the (smoothed) scrolling come to rest.
    h.run_steps(60);
}

/// Fold (or unfold) a category of the tree by clicking its header.
fn toggle(h: &mut Harness<'_, MeshFixerApp>, header: &str) {
    let pos = row_pos(h, header);
    h.event(egui::Event::PointerMoved(pos));
    h.step();
    press(h, pos, true);
    press(h, pos, false);
    h.run_steps(4);
}

#[test]
fn checkpoint_b_followup_real_scan() {
    if !mf_render::offscreen::gpu_available() {
        mf_fixtures::skip::gpu_unavailable("checkpoint_b_followup_real_scan");
        return;
    }
    let Some(path) =
        mf_fixtures::real::path("archaeopteryx.obj", "checkpoint_b_followup_real_scan")
    else {
        return;
    };
    let mut h = Harness::builder()
        .with_size(WINDOW)
        .with_step_dt(1.0 / FPS as f32)
        .with_max_steps(64)
        .wgpu()
        .build_eframe(|cc| MeshFixerApp::new(cc, None).unwrap());
    h.state_mut()
        .import_model(mf_io::read_model(&path).unwrap());
    wait_until(
        &mut h,
        "analysis and picking BVH",
        Duration::from_secs(600),
        |a| a.current_analysis().is_some() && a.shows_current_revision() && a.picking_ready(),
    );
    let mut rec = Recorder::new();
    key(&mut h, egui::Key::Home);
    h.run_steps(4);
    let defects = h.state().current_analysis().unwrap().defects.clone();
    let units = h.state().session.document().units();
    let row = |d: &Defect| defect_copy(d, units).row;
    println!(
        "scan: {} holes, {} cracks, {} loose pieces, {} tangled edges",
        defects.holes.len(),
        defects.cracks.len(),
        defects.loose.len(),
        defects.tangled.len()
    );
    rec.still(&mut h, "01-scan");

    // 1. The smallest loose piece listed on screen: fold the holes and
    //    cracks away so the pieces' rows show.
    for (name, n) in [
        ("Holes", defects.holes.len()),
        ("Narrow cracks", defects.cracks.len()),
    ] {
        if n > 0 {
            toggle(&mut h, &format!("{name} ({n})"));
        }
    }
    let size = |d: &Defect| match d {
        Defect::LoosePiece { size, .. } => size.unwrap_or(f64::MAX),
        _ => f64::MAX,
    };
    let piece = defects
        .loose
        .iter()
        .filter(|d| h.query_by_label(&row(d)).is_some())
        .min_by(|a, b| size(a).total_cmp(&size(b)))
        .expect("a loose piece on screen")
        .clone();
    println!("piece: {}", row(&piece));
    let piece_points = points(h.state(), &piece);
    let (_, before) = on_screen(h.state(), &piece_points);
    double_click(&mut h, &mut rec, &row(&piece));
    let Defect::LoosePiece { component, .. } = piece else {
        unreachable!()
    };
    let faces = h.state().active_mesh().unwrap().topology().components[component as usize].clone();
    wait_until(&mut h, "piece selected", SHORT, |a| {
        faces
            .iter()
            .all(|&f| a.read_face_state(f).unwrap().unwrap() & FaceStates::SELECTED != 0)
    });
    let (off, after) = on_screen(h.state(), &piece_points);
    println!("piece on screen: {before:.4} → {after:.3} of the view height, {off:.3} off centre");
    assert!(off < 0.05 && after >= 0.25, "framed: {off} {after}");
    h.event(egui::Event::PointerGone);
    h.run_steps(3);
    rec.still(&mut h, "02-tiny-loose-piece-framed");

    // 2. A hole on the far side from the default view.
    key(&mut h, egui::Key::Home);
    h.run_steps(4);
    let n = defects.loose.len();
    toggle(&mut h, &format!("Loose pieces ({n})"));
    toggle(&mut h, &format!("Holes ({})", defects.holes.len()));
    let view_dir = h.state().camera.unwrap().view_dir();
    let away = |d: &Defect| {
        defects
            .place(d)
            .and_then(|p| p.facing)
            .map_or(1.0, |f| f.dot(view_dir))
    };
    // The biggest of the holes that face well away from the view (so it
    // shows at the size it's framed at).
    let perimeter = |d: &Defect| match d {
        Defect::Hole { perimeter, .. } => perimeter.unwrap_or(0.0),
        _ => 0.0,
    };
    let hole = defects
        .holes
        .iter()
        .take(200)
        .filter(|d| matches!(d, Defect::Hole { .. }) && away(d) < -0.3)
        .max_by(|a, b| perimeter(a).total_cmp(&perimeter(b)))
        .expect("a hole on the far side")
        .clone();
    println!("hole: {} (facing · view = {:.2})", row(&hole), away(&hole));
    scroll_to(&mut h, &row(&hole));
    let hole_points = points(h.state(), &hole);
    double_click(&mut h, &mut rec, &row(&hole));
    let cam = h.state().camera.unwrap();
    let facing = defects.place(&hole).unwrap().facing.unwrap();
    assert!(
        cam.view_dir().dot(facing) > 0.0,
        "seen from the side it faces"
    );
    let (off, after) = on_screen(h.state(), &hole_points);
    println!("hole on screen: {after:.3} of the view height, {off:.3} off centre");
    assert!(off < 0.05 && after >= 0.25, "framed: {off} {after}");
    h.event(egui::Event::PointerGone);
    h.run_steps(3);
    rec.still(&mut h, "03-far-side-hole-framed");

    // 3. The object's row: the whole scan, framed.
    let name = h.state().session.document().objects()[0].name.clone();
    scroll_to(&mut h, &name);
    double_click(&mut h, &mut rec, &name);
    let all: Vec<DVec3> = {
        let app = h.state();
        let object = &app.session.document().objects()[0];
        let model = model_matrix(&object.transform);
        object
            .mesh()
            .unwrap()
            .vertices()
            .map(|(_, p)| model.transform_point3(DVec3::from(p)))
            .collect()
    };
    let (off, size) = on_screen(h.state(), &all);
    println!("object on screen: {size:.3} of the view height, {off:.3} off centre");
    assert!(
        off < 0.05 && (0.4..=1.2).contains(&size),
        "framed: {off} {size}"
    );
    // The pointer leaves the row: the object's hover glow goes.
    h.event(egui::Event::PointerGone);
    let face = h.state().active_mesh().unwrap().faces().next().unwrap().0.0;
    wait_until(&mut h, "hover glow gone", SHORT, |a| {
        a.read_face_state(face).unwrap().unwrap() & FaceStates::HOVER == 0
    });
    rec.still(&mut h, "04-object-row-framed");
    rec.finish();
}
