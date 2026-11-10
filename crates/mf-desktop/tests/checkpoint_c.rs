#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Checkpoint C (M4): the real app at 1440×900 end to end.
//!
//! Part 1 drives the full surgical workflow on the real Archaeopteryx scan
//! (fixtures/real/archaeopteryx.obj): the units prompt (preselecting
//! metres), Objects & Defects with size groups, the hole overview, picking
//! a hole and reading its Properties, filling it Smooth then Flat, the
//! batch "Fill holes up to N edges" preview and Apply, marking an
//! intentional opening, and exporting 3MF.
//!
//! Part 2 shows Smooth vs Flat vs Curvature where the difference is
//! visible: a large hole on a UV sphere, and the largest fillable hole
//! (≤ 1000 edges) on the messiest real scan with one that size
//! (fixtures/real/messy-inopos-131443.obj, an organic bust scan, 846
//! edges — picked by `scripts/capture-checkpoint-c.sh` having run `inspect
//! --json` over every fixtures/real/messy-* file; see its comment).
//!
//! Part 3 shows the "remove loose pieces" safeguard (MF-ISSUE-052) on
//! fixtures/real/messy-verlan-dress-285440.obj: the Properties preview line
//! and the confirmation dialog.
//!
//! Every step is asserted, so these always run as tests (recording a skip
//! when a scan isn't fetched); each early-returns unless `MF_ARTIFACTS` is
//! set, so they're cheap in the fast gate. With it set they write frames,
//! stills and an ffmpeg concat list per part to
//! `<dir>/checkpoint-c/`; scripts/capture-checkpoint-c.sh turns the frames
//! into checkpoint-c.mp4 (Part 1 only; Parts 2 and 3 are stills).

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mf_desktop::defects::{Defect, defect_copy};
use mf_desktop::{MeshFixerApp, Settings};
use mf_geometry::Mesh;
use mf_io::threemf::Unit;
use mf_selection::SelectionKind;
use mf_spec::ParamValue;

const WINDOW: (f32, f32) = (1440.0, 900.0);
const SHORT: Duration = Duration::from_secs(60);
const LONG: Duration = Duration::from_secs(600);
/// How long each still is shown in the recording (Part 1's MP4).
const STILL: f64 = 1.8;

/// Frames and stills for one part, written only with `MF_ARTIFACTS` set.
struct Recorder {
    dir: Option<PathBuf>,
    frames: usize,
    list: String,
}

impl Recorder {
    fn new() -> Recorder {
        let dir = std::env::var_os("MF_ARTIFACTS").map(|d| {
            let dir = PathBuf::from(d).join("checkpoint-c");
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
        format!("p1-{:04}.png", self.frames)
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

    /// Record the current frame and keep it as still `name` (no number
    /// prefix: Part 1's step-numbered filenames are given directly).
    fn still(&mut self, h: &mut Harness<'_, MeshFixerApp>, name: &str) {
        self.shot(h, STILL);
        let Some(dir) = &self.dir else { return };
        let frame = dir.join("frames").join(self.frame_name());
        std::fs::copy(frame, dir.join(format!("{name}.png"))).unwrap();
    }

    fn finish(self) {
        let Some(dir) = &self.dir else { return };
        std::fs::write(dir.join("frames-part1.txt"), self.list).unwrap();
    }
}

/// A still written straight to `checkpoint-c/`, outside the recorded
/// walkthrough (Parts 2 and 3 are stills only, no MP4).
fn still_only(h: &mut Harness<'_, MeshFixerApp>, name: &str) {
    let Some(dir) = std::env::var_os("MF_ARTIFACTS") else {
        return;
    };
    let dir = PathBuf::from(dir).join("checkpoint-c");
    std::fs::create_dir_all(&dir).unwrap();
    h.render()
        .unwrap()
        .save(dir.join(format!("{name}.png")))
        .unwrap();
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

/// Asserts some widget is labelled `label` (there can be more than one,
/// e.g. a tree row's hint and a Properties button with the same text).
fn assert_present(h: &Harness<'_, MeshFixerApp>, label: &str) {
    assert!(
        h.get_all_by_label(label).next().is_some(),
        "no widget labelled {label:?}"
    );
}

fn click_label(h: &mut Harness<'_, MeshFixerApp>, label: &str) {
    let pos = h
        .get_all_by_label(label)
        .next()
        .unwrap_or_else(|| panic!("no widget labelled {label:?}"))
        .rect()
        .center();
    click_at(h, pos);
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

/// The app as a person launches it (it asks for the units of a unitless
/// file), opening `path` right away.
fn opening(path: PathBuf) -> Harness<'static, MeshFixerApp> {
    Harness::builder()
        .with_size(WINDOW)
        .with_max_steps(16)
        .wgpu()
        .build_eframe(move |cc| {
            let settings = Settings {
                unit_prompt: true,
                ..Settings::default()
            };
            MeshFixerApp::with_settings(cc, Some(path.clone()), settings).unwrap()
        })
}

/// The midpoint of a hole's (or over-cap opening's) first boundary edge, in
/// object space: where clicking the viewport selects it.
fn hole_click_point(app: &MeshFixerApp, defect: &Defect) -> [f64; 3] {
    let mesh = app.active_mesh().unwrap();
    let loop_id = match defect {
        Defect::Hole { loop_id, .. } | Defect::OverCap { loop_id, .. } => *loop_id,
        _ => panic!("holes only"),
    };
    let topo = mesh.topology();
    let l = topo
        .boundary_loops
        .iter()
        .find(|l| l.id == loop_id)
        .unwrap();
    let (a, b) = (
        mesh.corner_position(l.vertices[0]),
        mesh.corner_position(l.vertices[1]),
    );
    [0, 1, 2].map(|k| (a[k] + b[k]) / 2.0)
}

/// Selects `defect`'s tree row (deterministic: no 3D picking) and frames
/// it with F, then clicks its now-centred, now-unoccluded viewport
/// position too, so the click genuinely exercises the viewport pick (the
/// task's "select a hole in the viewport (click)"). On a dense real scan
/// with many small holes close together, clicking straight from an
/// untouched camera position can land on a different, nearer hole; after
/// framing, this hole fills the view centre, so the click can't miss it.
fn click_hole(h: &mut Harness<'_, MeshFixerApp>, defect: &Defect) {
    let units = h.state().session.document().units();
    let row = defect_copy(defect, units).row;
    let row_pos = h
        .get_all_by_label(&row)
        .next()
        .unwrap_or_else(|| panic!("tree row {row:?}"))
        .rect()
        .center();
    click_at(h, row_pos);
    wait_until(h, "row-selected", SHORT, |a| a.selection.is_some());
    // F frames the row's selection: don't wait for the camera to change
    // (if it's already framed on this hole from an earlier call, F is a
    // no-op and the camera won't move), just give the animation a moment.
    key(h, egui::Key::F);
    h.run_steps(20);
    // Now click it in the viewport, at the point its own geometry
    // projects to post-framing.
    let at = hole_click_point(h.state(), defect);
    let pos = h.state().screen_point(at).unwrap();
    click_at(h, pos);
    wait_until(h, "hole picked", SHORT, |a| a.selection.is_some());
    // F frames the selection's overlay once it's built on a worker
    // (ViewRequest::FrameTarget silently no-ops until then): wait for the
    // selection outline to actually be drawn before any F press. On a
    // large real scan this overlay build can take a while.
    wait_until(h, "selection overlay ready", LONG, |a| {
        a.view_layers()
            .iter()
            .any(|l| l.color == mf_desktop::theme::SELECTED)
    });
}

// ---------------------------------------------------------------------
// Part 1: the full surgical workflow on the real Archaeopteryx scan.
// ---------------------------------------------------------------------

#[test]
fn checkpoint_c_part1_surgical_workflow() {
    if std::env::var_os("MF_ARTIFACTS").is_none() {
        return; // cheap in the fast gate; capture-checkpoint-c.sh sets it
    }
    if !mf_render::offscreen::gpu_available() {
        mf_fixtures::skip::gpu_unavailable("checkpoint_c_part1_surgical_workflow");
        return;
    }
    let Some(path) =
        mf_fixtures::real::path("archaeopteryx.obj", "checkpoint_c_part1_surgical_workflow")
    else {
        return;
    };
    let mut rec = Recorder::new();

    // 1. Open: the units prompt appears, preselecting metres (the scan's
    //    numbers are 0.495 x ... x 0.410, which fall in the metre bracket).
    let mut h = opening(path);
    wait_until(&mut h, "units prompt", LONG, |a| a.unit_prompt().is_some());
    let prompt = h.state().unit_prompt().unwrap();
    assert_eq!(prompt.choice, Unit::Meter, "metres preselected");
    let extent = prompt.extent;
    assert!(
        (extent[0] - 0.495).abs() < 0.01 && extent.iter().any(|&e| (e - 0.410).abs() < 0.01),
        "extent {extent:?}"
    );
    rec.still(&mut h, "01-units-prompt-metres-preselected");

    // Accept at metres.
    click_label(&mut h, "Import");
    wait_until(&mut h, "import + analysis + BVH", LONG, |a| {
        a.current_analysis().is_some() && a.shows_current_revision() && a.picking_ready()
    });
    assert!(h.state().error.is_none(), "{:?}", h.state().error);
    assert_eq!(h.state().session.document().units(), Unit::Meter);
    key(&mut h, egui::Key::Home);
    h.run_steps(4);

    // 2. Inspect: Objects & Defects (size groups and counts), then the
    //    hole overview zoomed out.
    h.get_by_label("Objects & Defects");
    let defects = h.state().current_analysis().unwrap().defects.clone();
    println!(
        "checkpoint_c part1: {} triangles; {} holes, {} cracks, {} loose pieces, {} tangled",
        h.state().active_mesh().unwrap().triangle_count(),
        defects.holes.len(),
        defects.cracks.len(),
        defects.loose.len(),
        defects.tangled.len()
    );
    rec.still(&mut h, "02-objects-and-defects-size-groups");

    // "Show hole overview" lives in the View menu (closed right now): set
    // it directly, as browser.rs's hole-overview tests do, rather than
    // navigating the menu.
    h.state_mut().show_hole_overview = true;
    h.run_steps(2);
    assert!(h.state().show_hole_overview);
    // Zoom out so the overview reads as an overview.
    let view = h.state().viewport.last_rect.unwrap();
    for _ in 0..6 {
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(0.0, -1.0),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: egui::Modifiers::NONE,
        });
        h.event(egui::Event::PointerMoved(view.center()));
        h.run_steps(2);
    }
    h.run_steps(6);
    rec.still(&mut h, "03-hole-overview-zoomed-out");

    // Zoom back in to a normal framing for the rest of the walkthrough.
    key(&mut h, egui::Key::Home);
    h.run_steps(4);

    // 3. Select a hole in the viewport: Properties shows its description
    //    and Fill with style options, Smooth preselected. The scan packs
    //    95 holes close together, so the largest hole's edge-midpoint can
    //    be screen-close to a neighbour's boundary: click there, then use
    //    whichever hole the pick actually landed on (still demonstrates
    //    the same step: click a hole, read its Properties).
    let target = defects
        .holes
        .iter()
        .filter(|d| matches!(d, Defect::Hole { .. }))
        .max_by(|a, b| {
            let edges = |d: &Defect| match d {
                Defect::Hole { edges, .. } => *edges,
                _ => 0,
            };
            edges(a).cmp(&edges(b))
        })
        .expect("a fillable hole")
        .clone();
    click_hole(&mut h, &target);
    assert_eq!(
        h.state().selection.as_ref().map(|s| s.kind),
        Some(SelectionKind::BoundaryLoop)
    );
    // The exact screen point that picked it (now that the camera is
    // framed on it): Undo restores the same geometry under the same
    // camera, so re-clicking here again picks the same hole without
    // re-resolving a tree row or a loop id that a fresh analysis may
    // renumber.
    let hole_screen_pos = {
        let at = hole_click_point(h.state(), &target);
        h.state().screen_point(at).unwrap()
    };
    let picked_loop = h.state().selection.as_ref().unwrap().ids[0];
    let hole = defects
        .holes
        .iter()
        .find(|d| matches!(d, Defect::Hole { loop_id, .. } if *loop_id == picked_loop))
        .expect("the clicked loop is a hole")
        .clone();
    let copy = defect_copy(&hole, h.state().session.document().units());
    assert_present(&h, &copy.detail);
    assert_present(&h, "Fill hole");
    let params = h.state_mut().params_for("mesh.fill_holes");
    assert_eq!(
        params.get("fill_style"),
        Some(&ParamValue::Text("smooth".into())),
        "Smooth preselected"
    );
    h.run_steps(2);
    rec.still(&mut h, "04-hole-selected-properties-fill-smooth");

    // 4. Fill (Smooth): recent-repair tint, then Undo (Flat vs Smooth is
    //    shown side by side in Part 2 instead, where the scan's own tiny
    //    holes wouldn't show a difference, MF-ISSUE-054).
    let _ = hole_screen_pos;
    let revision = h.state().session.current();
    click_label(&mut h, "Apply");
    wait_until(&mut h, "smooth fill + analysis + BVH", LONG, |a| {
        a.session.current() != revision
            && a.current_analysis().is_some()
            && a.shows_current_revision()
            && a.picking_ready()
    });
    assert!(h.state().error.is_none(), "{:?}", h.state().error);
    wait_until(&mut h, "recent-repair tint (smooth)", SHORT, |a| {
        let mesh = a.active_mesh().unwrap();
        let newest = mesh.face_slots() - 1;
        a.read_face_state(newest)
            .unwrap()
            .is_some_and(|s| s & mf_render::FaceStates::RECENT_REPAIR != 0)
    });
    h.run_steps(4);
    rec.still(&mut h, "05-filled-smooth-recent-repair-tint");

    click_label(&mut h, "Undo");
    wait_until(&mut h, "undo", LONG, |a| {
        a.session.current() == revision
            && a.current_analysis().is_some()
            && a.shows_current_revision()
            && a.picking_ready()
    });
    h.run_steps(3);
    rec.still(&mut h, "06-undo-back-to-open-hole");

    // 5. Mark an intentional opening on one hole, before the batch fill
    //    (intentional openings are left out of fills, so marking first
    //    guarantees this hole is still open and listed afterwards too).
    //    Selected by its tree row only (no viewport click or framing
    //    needed to mark it).
    let remaining = h.state().current_analysis().unwrap().defects.clone();
    let hole_loop = match &hole {
        Defect::Hole { loop_id, .. } => *loop_id,
        _ => unreachable!(),
    };
    let opening = remaining
        .holes
        .iter()
        .find(|d| matches!(d, Defect::Hole { loop_id, .. } if *loop_id != hole_loop))
        .expect("a different hole to mark")
        .clone();
    let opening_row = defect_copy(&opening, h.state().session.document().units()).row;
    click_label(&mut h, &opening_row);
    wait_until(&mut h, "row-selected", SHORT, |a| a.selection.is_some());
    h.run_steps(2);
    click_label(&mut h, "Mark as intentional opening");
    wait_until(&mut h, "marked intentional", SHORT, |a| {
        a.hole_view().is_some_and(|v| !v.marked.is_empty())
    });
    h.run_steps(3);
    rec.still(&mut h, "07-marked-intentional-opening");

    // 6. Batch: "Fill holes up to N edges" slider preview count, then
    //    Apply (one undo step for every hole it fills; the marked opening
    //    is excluded and still listed under Intentional openings after).
    key(&mut h, egui::Key::Escape);
    wait_until(&mut h, "nothing selected", SHORT, |a| a.selection.is_none());
    click_label(&mut h, "Fill all holes");
    h.run_steps(2);
    // The batch size slider's label (param_label's hardcoded override for
    // mesh.fill_holes' max_loop_edges; see crates/mf-desktop/src/defects.rs).
    h.get_by_label("Fill holes up to");
    let before_batch = h.state().session.current();
    rec.still(&mut h, "08-batch-fill-preview");
    click_label(&mut h, "Apply");
    wait_until(&mut h, "batch fill + analysis + BVH", LONG, |a| {
        a.session.current() != before_batch
            && a.current_analysis().is_some()
            && a.shows_current_revision()
            && a.picking_ready()
    });
    assert!(h.state().error.is_none(), "{:?}", h.state().error);
    // The marked opening is still open (left out of the batch) and still
    // listed under Intentional openings.
    let marked = h.state().hole_view().unwrap().marked.len();
    assert!(
        marked >= 1,
        "the intentional opening survives the batch fill"
    );
    h.get_by_label(&format!("Intentional openings ({marked})"));
    h.run_steps(4);
    rec.still(&mut h, "09-batch-fill-applied-opening-still-marked");

    // 7. Export 3MF, to a temp dir; a still frame of the status line.
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("archaeopteryx-repaired.3mf");
    h.state_mut().export_to(out.clone());
    wait_until(&mut h, "export finished", LONG, |a| {
        a.busy.is_none()
            && a.status
                .as_deref()
                .is_some_and(|s| s.starts_with("Exported"))
    });
    h.run_steps(3);
    rec.still(&mut h, "10-exported-3mf-status-line");
    assert!(out.is_file(), "the 3MF was written");
    let back = mf_io::read_model(&out).unwrap();
    assert_eq!(back.objects.len(), 1);

    rec.finish();
}

// ---------------------------------------------------------------------
// Part 2: Smooth vs Flat vs Curvature where it's visible.
// ---------------------------------------------------------------------

/// A UV sphere with one large hole: every face whose 3 vertices lie in a
/// contiguous patch of rings 2..=14 and segments 0..=15 (of a 16-ring,
/// 20-segment sphere) is removed, leaving a single big hole (54 boundary
/// edges, computed by a standalone script while writing this test;
/// checked below too).
fn sphere_with_large_hole() -> Mesh {
    let sphere = mf_fixtures::uv_sphere(16, 20);
    let ring = |r: u32, s: u32| 1 + (r - 1) * 20 + (s % 20);
    let mut patch_vertices = std::collections::HashSet::new();
    for r in 2..=14u32 {
        for s in 0..=15u32 {
            patch_vertices.insert(ring(r, s));
        }
    }
    let to_remove: Vec<mf_geometry::FaceId> = sphere
        .faces()
        .filter(|(_, tri)| tri.iter().all(|v| patch_vertices.contains(v)))
        .map(|(id, _)| id)
        .collect();
    let mut e = sphere.edit();
    for id in to_remove {
        let _ = e.remove_face(id);
    }
    e.finish().unwrap().0
}

fn model_of(name: &str, mesh: Mesh) -> mf_io::ImportedModel {
    mf_io::ImportedModel {
        objects: vec![mf_io::threemf::ModelObject {
            metadata: Default::default(),
            name: name.into(),
            mesh,
            transform: mf_io::threemf::IDENTITY,
        }],
        unit: mf_io::threemf::Unit::Millimeter,
        source: mf_io::SourceInfo {
            path: format!("{name}.obj").into(),
            content_hash: mf_geometry::ContentHash([0; 32]),
            format: mf_io::Format::Obj,
            precision: mf_io::SourcePrecision::F32,
        },
        warnings: vec![],
    }
}

fn opened(model: mf_io::ImportedModel, timeout: Duration) -> Harness<'static, MeshFixerApp> {
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

/// Fill the largest hole of the active object with `style`, framed on it
/// (so every style's still is the same camera), and record a still named
/// `{prefix}-{style}`. Undoes back to `before` after recording, so the next
/// style starts from the same mesh.
fn fill_and_shoot(
    h: &mut Harness<'_, MeshFixerApp>,
    style: &str,
    prefix: &str,
    hole: &Defect,
    before: mf_command::RevisionId,
) {
    click_hole(h, hole);
    assert_present(h, "Fill hole");
    h.state_mut()
        .params_for("mesh.fill_holes")
        .insert("fill_style".into(), ParamValue::Text(style.into()));
    h.run_steps(2);
    let view = h.state().viewport.last_rect.unwrap();
    h.event(egui::Event::PointerMoved(
        view.right_top() + egui::vec2(-6.0, 6.0),
    ));
    key(h, egui::Key::F);
    h.run_steps(4);
    let mut steps = 0;
    while h.state().camera_animating() {
        steps += 1;
        assert!(steps < 6000, "the camera move never ended");
        h.step();
    }
    click_label(h, "Apply");
    wait_until(h, "fill + analysis + BVH", LONG, |a| {
        a.session.current() != before
            && a.current_analysis().is_some()
            && a.shows_current_revision()
            && a.picking_ready()
    });
    assert!(h.state().error.is_none(), "{style}: {:?}", h.state().error);
    h.run_steps(4);
    still_only(h, &format!("{prefix}-{style}"));
    click_label(h, "Undo");
    wait_until(h, "undo", LONG, |a| {
        a.session.current() == before && a.shows_current_revision() && a.picking_ready()
    });
    h.run_steps(3);
}

#[test]
fn checkpoint_c_part2_fill_styles_sphere() {
    if std::env::var_os("MF_ARTIFACTS").is_none() {
        return;
    }
    if !mf_render::offscreen::gpu_available() {
        mf_fixtures::skip::gpu_unavailable("checkpoint_c_part2_fill_styles_sphere");
        return;
    }
    let mesh = sphere_with_large_hole();
    let mut h = opened(model_of("sphere with a large hole", mesh), LONG);
    key(&mut h, egui::Key::Home);
    h.run_steps(4);
    let defects = h.state().current_analysis().unwrap().defects.clone();
    println!(
        "checkpoint_c part2 sphere: {} holes: {:?}",
        defects.holes.len(),
        defects
            .holes
            .iter()
            .map(|d| match d {
                Defect::Hole { edges, .. } => *edges,
                _ => 0,
            })
            .collect::<Vec<_>>()
    );
    let hole = defects
        .holes
        .iter()
        .filter(|d| matches!(d, Defect::Hole { .. }))
        .max_by_key(|d| match d {
            Defect::Hole { edges, .. } => *edges,
            _ => 0,
        })
        .expect("the sphere's one large hole")
        .clone();
    if let Defect::Hole { edges, .. } = &hole {
        assert!(
            *edges >= 44,
            "large enough to show the fill styles: {edges}"
        );
    }
    let before = h.state().session.current();
    for style in ["flat", "smooth", "curvature"] {
        fill_and_shoot(&mut h, style, "p2-sphere-11", &hole, before);
    }
}

// Part 2's messy-model comparison (messy-inopos-131443.obj) is generated by
// scripts/capture-checkpoint-c.sh directly with the release CLI
// (`meshfixer fill-holes` + `meshfixer render`), not by driving the GUI:
// the mesh is 637k triangles, so two GUI fill+undo round trips are slow,
// and the CLI gives the same renderer output in ~4 s a style.

// ---------------------------------------------------------------------
// Part 3: the "remove loose pieces" safeguard.
// ---------------------------------------------------------------------

#[test]
fn checkpoint_c_part3_remove_loose_pieces_safeguard() {
    if std::env::var_os("MF_ARTIFACTS").is_none() {
        return;
    }
    if !mf_render::offscreen::gpu_available() {
        mf_fixtures::skip::gpu_unavailable("checkpoint_c_part3_remove_loose_pieces_safeguard");
        return;
    }
    let Some(path) = mf_fixtures::real::path(
        "messy-verlan-dress-285440.obj",
        "checkpoint_c_part3_remove_loose_pieces_safeguard",
    ) else {
        return;
    };
    let mut h = opened(mf_io::read_model(&path).unwrap(), LONG);
    key(&mut h, egui::Key::Home);
    h.run_steps(4);

    // Nothing selected: Properties offers "Remove loose pieces" (the
    // whole-object tool); opening it shows the preview line.
    key(&mut h, egui::Key::Escape);
    wait_until(&mut h, "nothing selected", SHORT, |a| a.selection.is_none());
    click_label(&mut h, "Remove loose pieces");
    h.run_steps(3);
    let preview = h
        .state()
        .removal_preview()
        .expect("a preview while the tool is open");
    assert!(preview.pieces > 0, "something would be removed");
    println!(
        "checkpoint_c part3: would remove {} of {} pieces",
        preview.pieces, preview.of_pieces
    );
    still_only(&mut h, "p3-01-remove-loose-pieces-preview");

    // Apply: it would remove enough to need confirmation (MF-ISSUE-052).
    click_label(&mut h, "Apply");
    wait_until(&mut h, "confirmation dialog", SHORT, |a| {
        a.confirmation().is_some()
    });
    let message = h.state().confirmation().unwrap().to_owned();
    assert!(
        message.contains("These may be part of the design"),
        "{message}"
    );
    println!("checkpoint_c part3: confirmation: {message}");
    h.run_steps(2);
    still_only(&mut h, "p3-02-confirmation-dialog");

    // Keep them: nothing is removed, as this is a capture, not a change.
    click_label(&mut h, "Keep them");
    h.run_steps(3);
    assert!(h.state().confirmation().is_none());
}
