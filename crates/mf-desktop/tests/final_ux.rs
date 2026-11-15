#![allow(clippy::unwrap_used)] // tests may panic on failure

//! M4 final UX review fixes (`.superpowers/sdd/2026-11-08-m4-scale-
//! reliability-ux/final-review-ux.md`): C1 (no dead ends: every defect says
//! what it means for printing, why MeshFixer can't repair it yet, and what
//! to do next), I1 (the curvature warning is never lost), I2 (import
//! warnings are readable), I3 (intentional openings have their own
//! colour), I4 (one name per thing), I5 (laptop-size windows), I6 (hole
//! markers on by default) and I7 (a pinched hole isn't offered Fill).
//!
//! With `MF_ARTIFACTS` set, each writes its "after" frame to
//! `<dir>/final-ux/<name>.png`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use mf_desktop::defects::{Defect, MARK_INTENTIONAL, METHOD_LIMIT, SELECT_ITS_FACES};
use mf_desktop::worker::{AnalysisRequest, Gpu, Message};
use mf_desktop::{MeshFixerApp, Settings};
use mf_geometry::{ContentHash, FaceId, Mesh};
use mf_io::threemf::{IDENTITY, ModelObject, Unit};
use mf_io::{Format, ImportedModel, SourceInfo, SourcePrecision};
use mf_runtime::{Cancelled, RunContext};
use mf_spec::ParamValue;

const WINDOW: (f32, f32) = (1440.0, 900.0);

macro_rules! require_gpu {
    ($name:literal) => {
        if !mf_render::offscreen::gpu_available() {
            mf_fixtures::skip::gpu_unavailable($name);
            return;
        }
    };
}

fn wait_until(h: &mut Harness<'_, MeshFixerApp>, what: &str, cond: impl Fn(&MeshFixerApp) -> bool) {
    let start = Instant::now();
    while !cond(h.state()) {
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "timed out waiting for: {what} (status {:?}, error {:?}, busy {:?})",
            h.state().status,
            h.state().error,
            h.state().busy
        );
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Steps until no background work is in flight and nothing asks for a
/// frame (on wall time, as layout.rs does), then a few more.
fn settle(h: &mut Harness<'_, MeshFixerApp>) {
    let start = Instant::now();
    loop {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "background work never finished"
        );
        h.step();
        if !h.state().background_work_in_flight() && !h.ctx.has_requested_repaint() {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    h.run_steps(3);
}

/// Saves the current frame as `<MF_ARTIFACTS>/final-ux/<name>.png`.
fn still(h: &mut Harness<'_, MeshFixerApp>, name: &str) {
    let Some(dir) = std::env::var_os("MF_ARTIFACTS") else {
        return;
    };
    let dir = PathBuf::from(dir).join("final-ux");
    std::fs::create_dir_all(&dir).unwrap();
    h.render()
        .unwrap()
        .save(dir.join(format!("{name}.png")))
        .unwrap();
}

fn model(name: &str, mesh: Mesh, warnings: Vec<String>) -> ImportedModel {
    ImportedModel {
        metadata: Default::default(),
        objects: vec![ModelObject {
            metadata: Default::default(),
            name: name.into(),
            mesh,
            transform: IDENTITY,
        }],
        unit: Some(Unit::Millimeter),
        source: SourceInfo {
            path: format!("{name}.3mf").into(),
            content_hash: ContentHash([0; 32]),
            format: Format::ThreeMf,
            precision: SourcePrecision::F32,
            exporter_hint: None,
        },
        warnings,
    }
}

fn harness(size: (f32, f32), settings: Settings) -> Harness<'static, MeshFixerApp> {
    Harness::builder()
        .with_size(size)
        .with_step_dt(1.0 / 60.0)
        .wgpu()
        .build_eframe(move |cc| MeshFixerApp::with_settings(cc, None, settings).unwrap())
}

fn open_in(
    mut h: Harness<'static, MeshFixerApp>,
    name: &str,
    mesh: Mesh,
    warnings: Vec<String>,
) -> Harness<'static, MeshFixerApp> {
    h.state_mut().import_model(model(name, mesh, warnings));
    wait_until(&mut h, "analysis, hole view and picking", |a| {
        a.current_analysis().is_some()
            && a.hole_view().is_some()
            && a.shows_current_revision()
            && a.picking_ready()
            && a.camera.is_some()
    });
    h.event(egui::Event::PointerGone);
    settle(&mut h);
    h
}

fn opened(name: &str, mesh: Mesh) -> Harness<'static, MeshFixerApp> {
    open_in(harness(WINDOW, Settings::default()), name, mesh, Vec::new())
}

/// A flat n×n-cell plate (cells `cell` mm) with a k×k-cell hole cut at each
/// `(i, j, k)` (its lower-left cell), plus its open rim (4n edges).
fn plate(n: u32, cell: f64, holes: &[(u32, u32, u32)]) -> Mesh {
    let grid = mf_fixtures::scaled(&mf_fixtures::grid(n), cell);
    let mut e = grid.edit();
    for &(i, j, k) in holes {
        for b in j..j + k {
            for a in i..i + k {
                let f = 2 * (b * n + a);
                e.remove_face(FaceId(f)).unwrap();
                e.remove_face(FaceId(f + 1)).unwrap();
            }
        }
    }
    e.finish().unwrap().0
}

fn labels(h: &Harness<'_, MeshFixerApp>) -> Vec<String> {
    h.query_all_by(|_| true)
        .filter_map(|n| n.accesskit_node().label())
        .collect()
}

fn assert_no_dead_end_words(h: &Harness<'_, MeshFixerApp>) {
    for l in labels(h) {
        for banned in [
            "not yet available",
            "current method",
            "Slit",
            "welding",
            "Non-manifold edges (",
            "island",
            "see the tree",
        ] {
            assert!(!l.contains(banned), "{banned:?} on screen: {l:?}");
        }
    }
}

fn click(h: &mut Harness<'_, MeshFixerApp>, label: &str) {
    h.get_by_label(label).click();
    h.run_steps(2);
}

/// Opens the Repair tool from the rail.
fn open_repair(h: &mut Harness<'_, MeshFixerApp>) {
    if h.state().active_tool != "repair" {
        click(h, "Repair");
    }
}

fn defects(h: &Harness<'_, MeshFixerApp>) -> std::sync::Arc<mf_desktop::defects::Defects> {
    h.state().current_analysis().unwrap().defects.clone()
}

/// C1: a hole over the fill method's limit (a scan's open base) says what
/// it means for printing and why it can't be filled (as a limit of this
/// fill method), and its next action, Mark as intentional opening, is the
/// primary button and works.
#[test]
fn a_very_large_hole_says_what_it_means_and_can_be_marked_intentional() {
    require_gpu!("a_very_large_hole_says_what_it_means_and_can_be_marked_intentional");
    // A 260-cell plate: its rim has 1,040 edges, over the 1,000-edge cap.
    let mut h = opened("base", plate(260, 0.5, &[]));
    let d = defects(&h);
    let base = d
        .holes
        .iter()
        .find(|h| matches!(h, Defect::OverCap { .. }))
        .unwrap()
        .clone();
    // Repair lists the size groups, with what each needs.
    open_repair(&mut h);
    assert!(
        h.get_all_by_label_contains("Very large holes (over 1000 edges)")
            .next()
            .is_some()
    );
    h.get_by_label_contains("Over this fill method's 1,000-edge limit");
    h.get_by_label_contains("Not watertight: 1 opening left");
    h.state_mut().select_defect(&base);
    settle(&mut h);
    h.get_by_label("Hole 1: very large opening");
    h.get_by_label_contains("often the open base or cut edge of a scan");
    h.get_by_label_contains(&format!(
        "{METHOD_LIMIT}: it fills holes of up to 1,000 edges"
    ));
    assert!(h.query_by_label("Fill hole").is_none());
    assert_no_dead_end_words(&h);
    still(&mut h, "c1-very-large-hole");
    click(&mut h, MARK_INTENTIONAL);
    settle(&mut h);
    assert_eq!(h.state().active_openings().len(), 1);
    h.get_by_label_contains("Intentional openings (1)");
    h.get_by_label_contains("Not watertight: 1 opening left (1 marked intentional)");
}

/// C1: a narrow crack says it's usually harmless and can be marked
/// intentional, so the count reaches zero; a tangled edge says it may
/// print wrongly and its faces can be selected to inspect, after which
/// Properties says what they are rather than "No operation works".
#[test]
fn cracks_and_tangled_edges_have_a_verdict_and_a_next_action() {
    require_gpu!("cracks_and_tangled_edges_have_a_verdict_and_a_next_action");
    let mut h = opened("plate", mf_fixtures::plate_with_defects());
    let d = defects(&h);
    h.get_by_label_contains("Has tangled edges: may print incorrectly");

    h.state_mut().select_defect(&d.cracks[0]);
    settle(&mut h);
    h.get_by_label("Narrow crack");
    h.get_by_label_contains("Usually harmless for printing");
    h.get_by_label_contains(&format!("{METHOD_LIMIT}: a crack has no area to fill"));
    assert_no_dead_end_words(&h);
    still(&mut h, "c1-narrow-crack");
    click(&mut h, MARK_INTENTIONAL);
    settle(&mut h);
    h.get_by_label("Narrow cracks (0)");
    h.get_by_label_contains("Intentional openings (1)");

    h.state_mut().select_defect(&d.tangled[0]);
    settle(&mut h);
    h.get_by_label("Tangled edge");
    h.get_by_label_contains("Slicers may print this spot wrongly");
    h.get_by_label_contains("repairing them is planned");
    still(&mut h, "c1-tangled-edge");
    click(&mut h, SELECT_ITS_FACES);
    settle(&mut h);
    h.get_by_label_contains("These triangles meet at a tangled edge");
    assert!(h.query_by_label_contains("No operation works").is_none());
    assert_no_dead_end_words(&h);
}

/// C1: a closed, manifold model says it's ready to print.
#[test]
fn a_watertight_model_says_it_is_ready_to_print() {
    require_gpu!("a_watertight_model_says_it_is_ready_to_print");
    let h = opened("cube", mf_fixtures::cube(10.0));
    h.get_by_label("Watertight: ready to print");
}

/// The real analysis, with the smallest hole reported pinched (passing
/// through one point twice), as a scan's can be.
fn analyze_with_a_pinched_hole(
    request: AnalysisRequest,
    ctx: &RunContext,
    gpu: &Gpu,
) -> Result<Message, Cancelled> {
    let openings = request.openings.clone();
    let mut msg = mf_desktop::worker::analyze(request, ctx, gpu)?;
    if let Message::Analyzed { analysis, .. } = &mut msg {
        let mut defects = (*analysis.defects).clone();
        let (_, id) = defects.holes.last().unwrap().selection().unwrap();
        defects.pinched = vec![id];
        analysis.hole_view =
            std::sync::Arc::new(mf_desktop::defects::hole_view(&defects, &openings));
        analysis.defects = std::sync::Arc::new(defects);
    }
    Ok(msg)
}

/// I7: a hole fill_holes would refuse (pinched) isn't offered Fill in
/// Properties any more than in the tree: Properties gives the reason and
/// Mark as intentional opening instead.
#[test]
fn a_pinched_hole_is_not_offered_fill_in_properties() {
    require_gpu!("a_pinched_hole_is_not_offered_fill_in_properties");
    let settings = Settings {
        analyze: Some(analyze_with_a_pinched_hole),
        ..Settings::default()
    };
    let mut h = open_in(
        harness(WINDOW, settings),
        "pinched",
        plate(12, 10.0, &[(2, 2, 1)]),
        Vec::new(),
    );
    let d = defects(&h);
    let pinched = d.holes.last().unwrap().clone();
    assert!(!d.fillable(&pinched));
    h.state_mut().select_defect(&pinched);
    settle(&mut h);
    assert!(h.state().tools().is_empty(), "{:?}", h.state().tools());
    assert!(h.query_by_label("Fill hole").is_none());
    h.get_by_label_contains("passes through one point twice");
    h.get_by_label(MARK_INTENTIONAL);
    still(&mut h, "i7-pinched-hole");
}

/// I4: one name per thing, plain term first: "Tangled edges" in the tree
/// and the legend (non-manifold in its tooltip), "Remove" on a loose
/// piece's row, holes titled by their size group.
#[test]
fn each_thing_has_one_name() {
    require_gpu!("each_thing_has_one_name");
    let mesh = mf_fixtures::merge(&[
        mf_fixtures::plate_with_defects(),
        mf_fixtures::translated(&mf_fixtures::cube(2.0), [60.0, 0.0, 0.0]),
    ]);
    let mut h = opened("plate", mesh);
    h.get_by_label("Tangled edges (1)");
    // The Overlays popover says the same.
    click(&mut h, "Overlays");
    h.get_by_label("Tangled edges");
    click(&mut h, "Overlays");
    // A loose piece's right-click menu: "Remove".
    let piece = defects(&h).loose[0].clone();
    let row = mf_desktop::defects::defect_copy(&piece, mf_io::threemf::Unit::Millimeter).row;
    h.get_by_label(&row).click_secondary();
    h.run_steps(3);
    h.get_by_label("Remove Piece 1");
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    // The punched one-cell hole: 4 edges, a tiny hole, a tiny opening.
    let d = defects(&h);
    let tiny = d
        .holes
        .iter()
        .find(|h| matches!(h, Defect::Hole { edges: 4, .. }))
        .unwrap()
        .clone();
    h.state_mut().select_defect(&tiny);
    settle(&mut h);
    h.get_by_label_contains(": tiny opening");
    assert_no_dead_end_words(&h);
    still(&mut h, "i4-one-name");
}

/// I3: a marked opening is drawn in its own colour over the hole outline,
/// never in the hole orange.
#[test]
fn intentional_openings_are_not_outlined_in_the_hole_colour() {
    require_gpu!("intentional_openings_are_not_outlined_in_the_hole_colour");
    let mut h = opened("plate", plate(12, 10.0, &[(4, 4, 3)]));
    let d = defects(&h);
    let inner = d
        .holes
        .iter()
        .find(|h| matches!(h, Defect::Hole { edges: 12, .. }))
        .unwrap()
        .clone();
    let (_, id) = inner.selection().unwrap();
    h.state_mut().mark_openings(&[id], true);
    wait_until(&mut h, "the openings layer", |a| {
        a.view_layers()
            .iter()
            .any(|l| l.color == mf_desktop::theme::OPENING)
    });
    h.event(egui::Event::PointerGone);
    settle(&mut h);
    // Sample along the opening's bottom edge (cells 4..7 at y = 4 cells).
    let image = h.render().unwrap();
    let (mut orange, mut slate) = (0, 0);
    for k in 1..10 {
        let x = 40.0 + 30.0 * f64::from(k) / 10.0;
        let p = h.state().screen_point([x, 40.0, 0.0]).unwrap();
        let px = image.get_pixel(p.x as u32, p.y as u32).0;
        let [r, g, b] = [0, 1, 2].map(|i| i32::from(px[i]));
        if r > 200 && g < 170 && b < 90 {
            orange += 1;
        }
        if b >= r && (r - 132).abs() < 40 && (b - 160).abs() < 40 {
            slate += 1;
        }
    }
    assert_eq!(orange, 0, "the opening is still drawn in the hole orange");
    assert!(slate >= 7, "{slate} of 9 points in the opening colour");
    still(&mut h, "i3-intentional-opening");
}

/// An 80-edge rim and a punched hole: the rim is over the 60 edges where
/// curvature-matched fill stays accurate (MF-ISSUE-055).
fn wide_plate() -> Mesh {
    plate(20, 5.0, &[(5, 5, 1)])
}

fn choose_curvature(h: &mut Harness<'_, MeshFixerApp>) {
    h.state_mut()
        .params_for("mesh.fill_holes")
        .insert("fill_style".into(), ParamValue::Text("curvature".into()));
    h.run_steps(2);
}

/// I1: before Apply, Properties warns that curvature matching is less
/// accurate on this hole; after a targeted fill the warning is still
/// readable in full (the "selection cleared" note doesn't replace it).
#[test]
fn the_curvature_warning_is_shown_before_and_survives_a_targeted_fill() {
    require_gpu!("the_curvature_warning_is_shown_before_and_survives_a_targeted_fill");
    let mut h = opened("plate", wide_plate());
    let d = defects(&h);
    let rim = d
        .holes
        .iter()
        .find(|h| matches!(h, Defect::Hole { edges: 80, .. }))
        .unwrap()
        .clone();
    h.state_mut().select_defect(&rim);
    settle(&mut h);
    // The card's "Fill hole" opens it in Repair.
    click(&mut h, "Fill hole");
    choose_curvature(&mut h);
    h.get_by_label_contains("Less accurate on holes over 60 edges (this one has 80)");
    still(&mut h, "i1-curvature-before-apply");
    h.get_by_label("Apply").click();
    wait_until(&mut h, "the fill", |a| {
        a.busy.is_none() && a.session.can_undo() && a.hole_view().is_some()
    });
    settle(&mut h);
    let warning = mf_operations::curvature_limit_warning(1);
    let notice = h.state().notice.clone().unwrap_or_default();
    assert!(notice.contains(&warning), "notice {notice:?}");
    // In Properties' "Last result", in full (and in the status line).
    h.get_by_label("Last result");
    assert!(h.get_all_by_label(&notice).count() >= 1);
    let status = h.state().status.clone().unwrap_or_default();
    assert!(!status.contains("selection cleared"), "{status}");
    still(&mut h, "i1-curvature-after-targeted-fill");
}

/// I1: after a batch fill, the warning is readable in full in Properties,
/// not cut off in the status line.
#[test]
fn the_curvature_warning_is_readable_in_full_after_a_batch_fill() {
    require_gpu!("the_curvature_warning_is_readable_in_full_after_a_batch_fill");
    let mut h = opened("plate", wide_plate());
    open_repair(&mut h);
    click(&mut h, "Fill all holes");
    choose_curvature(&mut h);
    h.get_by_label_contains("Less accurate on holes over 60 edges (1 of these is)");
    h.get_by_label("Apply").click();
    wait_until(&mut h, "the fill", |a| {
        a.busy.is_none() && a.session.can_undo() && a.notice.is_some()
    });
    settle(&mut h);
    let warning = mf_operations::curvature_limit_warning(1);
    let notice = h.state().notice.clone().unwrap();
    assert!(notice.contains(&warning), "notice {notice:?}");
    h.get_by_label("Last result");
    assert!(h.get_all_by_label(&notice).count() >= 1);
    still(&mut h, "i1-curvature-after-batch-fill");
    click(&mut h, "Dismiss note");
    assert!(h.state().notice.is_none());
}

/// I2: import warnings are listed, grouped with counts, behind a button
/// in the status bar that outlasts later status messages, and in
/// Properties' Details.
#[test]
fn import_warnings_can_be_read() {
    require_gpu!("import_warnings_can_be_read");
    let mut warnings = vec!["degenerate face 7 skipped".to_owned(); 1];
    warnings.extend(vec!["a zero-area face was skipped".to_owned(); 3]);
    warnings.push("read as millimetres (the file has no units)".to_owned());
    let mut h = open_in(
        harness(WINDOW, Settings::default()),
        "warned",
        mf_fixtures::cube(10.0),
        warnings,
    );
    // A later status message doesn't hide them.
    h.state_mut().status = Some("Something else happened".into());
    h.run_steps(2);
    click(&mut h, "5 import warnings");
    settle(&mut h);
    h.get_by_label("3 × a zero-area face was skipped");
    h.get_by_label("read as millimetres (the file has no units)");
    still(&mut h, "i2-import-warnings");
    // And in Inspect.
    click(&mut h, "Inspect");
    h.get_by_label_contains("Import warnings (5)");
}

/// I6: hole markers are on by default, so a model's tiny holes show in the
/// view without turning anything on (the neighbourhood tint stays off).
#[test]
fn hole_markers_are_on_by_default() {
    require_gpu!("hole_markers_are_on_by_default");
    let mut h = opened("speckled", plate(200, 1.0, &[(40, 40, 1), (150, 120, 1)]));
    assert!(h.state().show_hole_markers);
    assert!(!h.state().show_hole_overview, "the tint stays opt-in");
    wait_until(&mut h, "markers", |a| {
        a.hole_markers().is_some_and(|m| !m.is_empty())
    });
    settle(&mut h);
    click(&mut h, "Overlays");
    h.get_by_label("Hole markers");
    still(&mut h, "i6-hole-markers");
}

/// I5: at laptop sizes the view keeps most of the window, the navigation
/// bar fits inside it without covering the controls hint, and the side
/// panels can be hidden (View > Hide side panels, Cmd/Ctrl+\) and brought
/// back.
#[test]
fn laptop_sizes_keep_the_view_dominant_and_the_panels_collapsible() {
    require_gpu!("laptop_sizes_keep_the_view_dominant_and_the_panels_collapsible");
    for (w, ht) in [(1280.0, 720.0), (1024.0, 640.0)] {
        let mut h = open_in(
            harness((w, ht), Settings::default()),
            "plate",
            plate(12, 10.0, &[(1, 1, 1), (5, 1, 2), (2, 6, 3)]),
            Vec::new(),
        );
        let name = format!("{}x{}", w as u32, ht as u32);
        let view = h.state().viewport.last_rect.unwrap();
        let share = view.width() / w;
        assert!(
            share >= 0.55,
            "{name}: the view {view:?} is {:.0}% wide",
            share * 100.0
        );
        let orbit = h.get_by_label("Orbit").rect();
        let reset = h.get_by_label("Reset view").rect();
        let bar = orbit.union(reset);
        assert!(
            bar.left() >= view.left() && bar.right() <= view.right(),
            "{name}: the navigation bar {bar:?} is clipped by the view {view:?}"
        );
        let hint = h.state().viewport.last_hint_rect.unwrap();
        assert!(
            !hint.intersects(bar),
            "{name}: the navigation bar {bar:?} covers the hint {hint:?}"
        );
        still(&mut h, &format!("i5-{name}-panels"));

        h.event(egui::Event::Key {
            key: egui::Key::Backslash,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND,
        });
        settle(&mut h);
        let wide = h.state().viewport.last_rect.unwrap();
        assert!(
            wide.width() / w >= 0.9,
            "{name}: with the panels hidden the view is {:.0}% wide",
            100.0 * wide.width() / w
        );
        still(&mut h, &format!("i5-{name}-panels-hidden"));
        // Cmd/Ctrl+\ again brings them back.
        h.event(egui::Event::Key {
            key: egui::Key::Backslash,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND,
        });
        settle(&mut h);
        h.get_by_label("Browser");
        let back = h.state().viewport.last_rect.unwrap();
        assert!(
            (back.width() - view.width()).abs() < 2.0,
            "{back:?} vs {view:?}"
        );
        // The Browser folds to a strip, and back.
        click(&mut h, "Hide Browser");
        settle(&mut h);
        assert!(h.query_by_label("Browser").is_none());
        click(&mut h, "Show Browser");
        settle(&mut h);
        h.get_by_label("Browser");
    }
}
