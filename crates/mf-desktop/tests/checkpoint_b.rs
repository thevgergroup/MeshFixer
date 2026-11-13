#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Checkpoint B (M4 Task 8): the real app at 1440×900 on the real
//! Archaeopteryx scan and on a 5M-triangle damaged mesh (generated in
//! memory, never written to disk): defect overlays; hovering a hole, a
//! loose piece and a tangled edge, with the size tooltip; clicking each,
//! with its Properties panel; and the recent-repair tint after a fill.
//! Every step is asserted, so this always runs as a test (the scan one
//! records a skip when the scan isn't fetched); with `MF_ARTIFACTS=<dir>`
//! it also writes the frames, stills and an ffmpeg concat list to
//! `<dir>/checkpoint-b/` (scripts/capture-checkpoint-b.sh turns them into
//! the MP4).

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mf_desktop::MeshFixerApp;
use mf_desktop::defects::{Defect, defect_copy};
use mf_desktop::worker::OverlayTarget;
use mf_geometry::{ContentHash, Mesh};
use mf_io::{Format, ImportedModel, SourceInfo, SourcePrecision};
use mf_render::FaceStates;
use mf_selection::SelectionKind;

const WINDOW: (f32, f32) = (1440.0, 900.0);
/// Frame rate of the pointer-motion parts of the recording.
const FPS: f64 = 15.0;
/// How long each still is shown in the recording.
const STILL: f64 = 1.6;

/// One walkthrough at a time: each holds a large mesh, and they share the
/// recording directory.
fn one_at_a_time() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

/// Frames and stills, written only with `MF_ARTIFACTS` set. Frame numbers
/// carry a prefix per walkthrough, so the two recordings play one after
/// the other (the concat list is sorted by name).
struct Recorder {
    dir: Option<PathBuf>,
    prefix: &'static str,
    frames: usize,
    list: String,
}

impl Recorder {
    fn new(prefix: &'static str) -> Recorder {
        let dir = std::env::var_os("MF_ARTIFACTS").map(|d| {
            let dir = PathBuf::from(d).join("checkpoint-b");
            std::fs::create_dir_all(dir.join("frames")).unwrap();
            dir
        });
        Recorder {
            dir,
            prefix,
            frames: 0,
            list: String::new(),
        }
    }

    fn frame_name(&self) -> String {
        format!("{}-{:04}.png", self.prefix, self.frames)
    }

    /// Record the current frame, shown for `seconds`.
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

    /// Record the current frame and keep it as still `name`.
    fn still(&mut self, h: &mut Harness<'_, MeshFixerApp>, name: &str) {
        self.shot(h, STILL);
        let Some(dir) = &self.dir else { return };
        let frame = dir.join("frames").join(self.frame_name());
        std::fs::copy(frame, dir.join(format!("{}-{name}.png", self.prefix))).unwrap();
    }

    /// This walkthrough's part of the concat list (the capture script
    /// joins the parts and repeats the very last file, as the concat
    /// demuxer needs to honour its duration).
    fn finish(self) {
        let Some(dir) = &self.dir else { return };
        std::fs::write(dir.join(format!("frames-{}.txt", self.prefix)), self.list).unwrap();
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

/// The app with `model` open, analysed and ready to pick (its BVH built).
fn opened(model: ImportedModel, timeout: Duration) -> Harness<'static, MeshFixerApp> {
    let mut h = Harness::builder()
        .with_size(WINDOW)
        .with_max_steps(16)
        .wgpu()
        .build_eframe(|cc| MeshFixerApp::new(cc, None).unwrap());
    h.state_mut().import_model(model);
    wait_until(&mut h, "analysis and picking BVH", timeout, |a| {
        a.current_analysis().is_some() && a.shows_current_revision() && a.picking_ready()
    });
    h
}

fn model_of(name: &str, mesh: Mesh) -> ImportedModel {
    ImportedModel {
        metadata: Default::default(),
        objects: vec![mf_io::threemf::ModelObject {
            metadata: Default::default(),
            name: name.into(),
            mesh,
            transform: mf_io::threemf::IDENTITY,
        }],
        unit: mf_io::threemf::Unit::Millimeter,
        source: SourceInfo {
            path: format!("{name}.obj").into(),
            content_hash: ContentHash([0; 32]),
            format: Format::Obj,
            precision: SourcePrecision::F32,
        },
        warnings: vec![],
    }
}

/// The point the pointer rests on to hover `defect` (object space): a
/// third of the way along a hole's or tangled edge's first edge (clear of
/// the edges meeting at its ends), or a loose piece's first face's centre.
fn hover_point(app: &MeshFixerApp, defect: &Defect) -> [f64; 3] {
    let mesh = app.active_mesh().unwrap();
    let third = |a: [f64; 3], b: [f64; 3]| [0, 1, 2].map(|k| a[k] + (b[k] - a[k]) / 3.0);
    match defect {
        Defect::Hole { loop_id, .. } | Defect::OverCap { loop_id, .. } => {
            let topo = mesh.topology();
            let l = topo
                .boundary_loops
                .iter()
                .find(|l| l.id == *loop_id)
                .unwrap();
            third(
                mesh.corner_position(l.vertices[0]),
                mesh.corner_position(l.vertices[1]),
            )
        }
        Defect::Tangled { edge, .. } => {
            third(mesh.corner_position(edge[0]), mesh.corner_position(edge[1]))
        }
        Defect::LoosePiece { component, .. } => {
            let topo = mesh.topology();
            let f = topo.components[*component as usize][0];
            let t = mesh.face(mf_geometry::FaceId(f)).unwrap();
            let p = t.map(|v| mesh.corner_position(v));
            [0, 1, 2].map(|k| (p[0][k] + p[1][k] + p[2][k]) / 3.0)
        }
        Defect::Crack { .. } => panic!("cracks aren't part of this walkthrough"),
    }
}

/// What hovering `defect` reports.
fn overlay_target(defect: &Defect) -> OverlayTarget {
    match defect {
        Defect::Hole { loop_id, .. } | Defect::OverCap { loop_id, .. } => {
            OverlayTarget::Loops(vec![*loop_id])
        }
        Defect::Tangled { edge, .. } => OverlayTarget::Edges(vec![*edge]),
        Defect::LoosePiece { component, .. } => OverlayTarget::Components(vec![*component]),
        Defect::Crack { .. } => unreachable!(),
    }
}

/// Frame `defect` (select it in the tree, F, then Esc so nothing is
/// selected), glide the pointer onto it and check the hover and its
/// tooltip; then click it in the view and check what's selected. Records
/// the hover and the selection with its Properties panel.
fn hover_and_click(
    h: &mut Harness<'_, MeshFixerApp>,
    rec: &mut Recorder,
    defect: &Defect,
    row: &str,
    name: &str,
) {
    let row_pos = h
        .get_all_by_label(row)
        .next()
        .unwrap_or_else(|| panic!("tree row {row}"))
        .rect()
        .center();
    click_at(h, row_pos);
    let view = h.state().viewport.last_rect.unwrap();
    h.event(egui::Event::PointerMoved(
        view.right_top() + egui::vec2(-6.0, 6.0),
    ));
    h.step();
    let before = h.state().camera.unwrap();
    key(h, egui::Key::F);
    wait_until(h, "framed", SHORT, |a| a.camera != Some(before));
    key(h, egui::Key::Escape);
    wait_until(h, "nothing selected", SHORT, |a| {
        a.selection.is_none() && a.current_inspected().is_none()
    });

    // Glide in from the side, so the recording shows the hover arriving.
    let at = hover_point(h.state(), defect);
    let target = h.state().screen_point(at).unwrap();
    let from = target + egui::vec2(-160.0, 90.0);
    for i in 0..=12 {
        let p = from + (target - from) * (i as f32 / 12.0);
        h.event(egui::Event::PointerMoved(p));
        h.step();
        rec.shot(h, 1.0 / FPS);
    }
    let want = overlay_target(defect);
    wait_until(h, "hover", SHORT, |a| {
        a.view_hover().is_some_and(|v| v.target == want)
    });
    let copy = defect_copy(defect, h.state().session.document().units());
    h.get_by_label(&copy.detail);
    if let OverlayTarget::Components(c) = &want {
        let faces = h.state().active_mesh().unwrap().topology().components[c[0] as usize].clone();
        wait_until(h, "hover tint", SHORT, |a| {
            faces
                .iter()
                .all(|&f| a.read_face_state(f).unwrap().unwrap() & FaceStates::HOVER != 0)
        });
    } else {
        wait_until(h, "hover outline", SHORT, |a| {
            a.view_layers()
                .iter()
                .any(|l| l.color == mf_desktop::theme::HOVER)
        });
    }
    rec.still(h, &format!("hover-{name}"));

    click_at(h, target);
    let expected = match defect {
        Defect::Hole { loop_id, .. } | Defect::OverCap { loop_id, .. } => {
            (SelectionKind::BoundaryLoop, vec![*loop_id])
        }
        Defect::LoosePiece { component, .. } => (SelectionKind::Component, vec![*component]),
        Defect::Tangled { faces, .. } => {
            let mut faces = faces.clone();
            faces.sort_unstable();
            (SelectionKind::Faces, faces)
        }
        Defect::Crack { .. } => unreachable!(),
    };
    wait_until(h, "clicked selection", SHORT, |a| {
        a.selection
            .as_ref()
            .is_some_and(|s| (s.kind, s.ids.clone()) == expected)
    });
    // The selection overlay (and tint) is in, and the pointer has moved
    // off so the still shows the selection, not the hover.
    h.event(egui::Event::PointerMoved(
        view.right_top() + egui::vec2(-6.0, 6.0),
    ));
    // A hole shows the selection outline; a piece or a tangled edge's
    // faces the selection tint.
    let selected_faces: Vec<u32> = match &expected {
        (SelectionKind::Component, c) => {
            h.state().active_mesh().unwrap().topology().components[c[0] as usize].clone()
        }
        (SelectionKind::Faces, faces) => faces.clone(),
        _ => Vec::new(),
    };
    wait_until(h, "selection drawn", SHORT, |a| {
        let layers = a.view_layers();
        a.view_hover().is_none()
            && layers.iter().all(|l| l.color != mf_desktop::theme::HOVER)
            && (if selected_faces.is_empty() {
                layers
                    .iter()
                    .any(|l| l.color == mf_desktop::theme::SELECTED)
            } else {
                selected_faces
                    .iter()
                    .all(|&f| a.read_face_state(f).unwrap().unwrap() & FaceStates::SELECTED != 0)
            })
    });
    h.run_steps(4);
    rec.still(h, &format!("clicked-{name}"));
}

/// Hover and click a hole, a loose piece and a tangled edge, then fill the
/// hole and show the recent-repair tint.
fn walkthrough(
    mut h: Harness<'static, MeshFixerApp>,
    rec: &mut Recorder,
    analysis_timeout: Duration,
) {
    key(&mut h, egui::Key::Home);
    h.run_steps(4);
    let defects = h.state().current_analysis().unwrap().defects.clone();
    println!(
        "{}: {} triangles; {} holes, {} cracks, {} loose pieces, {} tangled edges",
        rec.prefix,
        h.state().active_mesh().unwrap().triangle_count(),
        defects.holes.len(),
        defects.cracks.len(),
        defects.loose.len(),
        defects.tangled.len()
    );
    rec.still(&mut h, "01-defect-overlays");

    // The biggest fillable hole, so it shows at the size it's framed at
    // (a scan's first hole can be a three-edge pinhole).
    let size = |d: &Defect| match d {
        Defect::Hole {
            edges, perimeter, ..
        } => (perimeter.unwrap_or(0.0), *edges),
        _ => (-1.0, 0),
    };
    let hole = defects
        .holes
        .iter()
        .filter(|d| matches!(d, Defect::Hole { .. }))
        .max_by(|a, b| size(a).partial_cmp(&size(b)).expect("finite perimeters"))
        .expect("a fillable hole")
        .clone();
    println!("{}: hole {:?}", rec.prefix, size(&hole));
    let hole_row = defect_copy(&hole, h.state().session.document().units()).row;
    hover_and_click(&mut h, rec, &hole, &hole_row, "02-hole");
    h.get_by_label("Fill hole");

    // The tree lists every hole and crack above the loose pieces: fold
    // them away so the pieces' rows are on screen.
    let header = |name: &str, n: usize| format!("{name} ({n})");
    let holes_header = header("Holes", defects.holes.len());
    for label in [
        &holes_header,
        &header("Narrow cracks", defects.cracks.len()),
    ] {
        let pos = h.get_by_label(label).rect().center();
        click_at(&mut h, pos);
    }
    h.run_steps(4);

    let piece = defects.loose.first().expect("a loose piece").clone();
    let piece_row = defect_copy(&piece, h.state().session.document().units()).row;
    hover_and_click(&mut h, rec, &piece, &piece_row, "03-loose-piece");
    h.get_by_label("Remove piece");

    let pos = h
        .get_by_label(&header("Loose pieces", defects.loose.len()))
        .rect()
        .center();
    click_at(&mut h, pos);
    h.run_steps(4);

    let tangled = defects.tangled.first().expect("a tangled edge").clone();
    let tangled_row = defect_copy(&tangled, h.state().session.document().units()).row;
    hover_and_click(&mut h, rec, &tangled, &tangled_row, "04-tangled-edge");

    // Fill the hole: select it in the tree, Apply; the patch is tinted as
    // a recent repair.
    let pos = h.get_by_label(&holes_header).rect().center();
    click_at(&mut h, pos);
    h.run_steps(4);
    let row_pos = h.get_by_label(&hole_row).rect().center();
    click_at(&mut h, row_pos);
    assert_eq!(
        h.state().selection.as_ref().map(|s| s.kind),
        Some(SelectionKind::BoundaryLoop)
    );
    let view = h.state().viewport.last_rect.unwrap();
    h.event(egui::Event::PointerMoved(
        view.right_top() + egui::vec2(-6.0, 6.0),
    ));
    let before = h.state().camera.unwrap();
    key(&mut h, egui::Key::F);
    wait_until(&mut h, "framed on the hole", SHORT, |a| {
        a.camera != Some(before)
    });
    // Select's card offers the target's operation; it opens Repair with
    // the hole as the target, where Apply is.
    let fill = h.get_by_label("Fill hole").rect().center();
    click_at(&mut h, fill);
    h.run_steps(4);
    let revision = h.state().session.current();
    let apply = h.get_by_label("Apply").rect().center();
    click_at(&mut h, apply);
    wait_until(&mut h, "fill, analysis and BVH", analysis_timeout, |a| {
        a.session.current() != revision
            && a.current_analysis().is_some()
            && a.shows_current_revision()
            && a.picking_ready()
    });
    assert!(h.state().error.is_none(), "{:?}", h.state().error);
    wait_until(&mut h, "recent-repair tint", SHORT, |a| {
        let mesh = a.active_mesh().unwrap();
        let newest = mesh.face_slots() - 1;
        a.read_face_state(newest)
            .unwrap()
            .is_some_and(|s| s & FaceStates::RECENT_REPAIR != 0)
    });
    h.run_steps(4);
    rec.still(&mut h, "05-recent-repair-after-fill");
}

#[test]
fn checkpoint_b_real_scan() {
    if !mf_render::offscreen::gpu_available() {
        mf_fixtures::skip::gpu_unavailable("checkpoint_b_real_scan");
        return;
    }
    let Some(path) = mf_fixtures::real::path("archaeopteryx.obj", "checkpoint_b_real_scan") else {
        return;
    };
    let _one = one_at_a_time();
    let mut model = mf_io::read_model(&path).unwrap();
    let object = &mut model.objects[0];
    object.name = "archaeopteryx (with a fin)".into();
    object.mesh = with_fin(object.mesh.clone());
    let h = opened(model, Duration::from_secs(600));
    let mut rec = Recorder::new("a-scan");
    walkthrough(h, &mut rec, Duration::from_secs(600));
    rec.finish();
}

/// `mesh` with a fin standing on one of its edges: a tangled edge (three
/// faces share it). Neither the scan nor the generated mesh has one of its
/// own.
fn with_fin(mesh: Mesh) -> Mesh {
    let (mut positions, mut triangles) = mesh.to_arrays();
    drop(mesh);
    let [a, b, c] = triangles[triangles.len() / 3];
    let p = [a, b, c].map(|v| glam::DVec3::from(positions[v as usize]));
    let normal = (p[1] - p[0]).cross(p[2] - p[0]).normalize();
    let tip = (p[0] + p[1]) * 0.5 + normal * (p[1] - p[0]).length() * 2.0;
    positions.push(tip.to_array());
    triangles.push([a, b, positions.len() as u32 - 1]);
    Mesh::new(positions, triangles).unwrap()
}

#[test]
fn checkpoint_b_5m_mesh() {
    if !mf_render::offscreen::gpu_available() {
        mf_fixtures::skip::gpu_unavailable("checkpoint_b_5m_mesh");
        return;
    }
    let _one = one_at_a_time();
    let sphere =
        mf_fixtures::damaged_sphere(&mf_fixtures::DamageSpec::for_triangles(5_000_000, 11));
    let h = opened(
        model_of("damaged 5M sphere (with a fin)", with_fin(sphere)),
        Duration::from_secs(600),
    );
    let mut rec = Recorder::new("b-5m");
    walkthrough(h, &mut rec, Duration::from_secs(600));
    rec.finish();
}
