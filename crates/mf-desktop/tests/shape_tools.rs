#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The Shape tools on the rail (UI plan step 5): Split, Combine and Make
//! solid, through `shell::Tool`. Each opens from the rail, previews in the
//! view, runs its operation on the job pool as one command, and shows a
//! refusal in plain words with the next action as a button.
//!
//! With `MF_ARTIFACTS` set, frames go to
//! `<dir>/design/tools-<scene>-<w>x<h>.png`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mf_desktop::{MeshFixerApp, Settings};
use mf_geometry::{ContentHash, Mesh};
use mf_io::threemf::{ModelObject, Unit};
use mf_io::{Format, ImportedModel, SourceInfo, SourcePrecision};
use mf_selection::{Selection, SelectionKind};

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
            start.elapsed() < Duration::from_secs(90),
            "timed out waiting for: {what} (status {:?}, error {:?}, refusal {:?})",
            h.state().status,
            h.state().error,
            h.state().refusal,
        );
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn settle(h: &mut Harness<'_, MeshFixerApp>) {
    let start = Instant::now();
    loop {
        assert!(start.elapsed() < Duration::from_secs(60), "never settled");
        h.step();
        if !h.state().background_work_in_flight() && !h.ctx.has_requested_repaint() {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    h.run_steps(3);
}

fn still(h: &mut Harness<'_, MeshFixerApp>, scene: &str) {
    let Some(dir) = std::env::var_os("MF_ARTIFACTS") else {
        return;
    };
    let dir = PathBuf::from(dir).join("design");
    std::fs::create_dir_all(&dir).unwrap();
    let size = h.ctx.content_rect().size();
    let name = format!("tools-{scene}-{}x{}.png", size.x as u32, size.y as u32);
    h.render().unwrap().save(dir.join(name)).unwrap();
}

fn opened(
    size: (f32, f32),
    objects: Vec<(&str, Mesh, [f64; 12])>,
) -> Harness<'static, MeshFixerApp> {
    let mut h = Harness::builder()
        .with_size(size)
        .with_step_dt(1.0 / 60.0)
        .wgpu()
        .build_eframe(move |cc| {
            MeshFixerApp::with_settings(cc, None, Settings::default()).unwrap()
        });
    h.state_mut().import_model(ImportedModel {
        objects: objects
            .into_iter()
            .map(|(name, mesh, transform)| ModelObject {
                name: name.into(),
                mesh,
                transform,
            })
            .collect(),
        unit: Unit::Millimeter,
        source: SourceInfo {
            path: "tools.3mf".into(),
            content_hash: ContentHash([0; 32]),
            format: Format::ThreeMf,
            precision: SourcePrecision::F32,
        },
        warnings: Vec::new(),
    });
    wait_until(&mut h, "analysis and picking", |a| {
        a.current_analysis().is_some()
            && a.hole_view().is_some()
            && a.picking_ready()
            && a.camera.is_some()
    });
    h.event(egui::Event::PointerGone);
    settle(&mut h);
    h
}

fn at(t: [f64; 3]) -> [f64; 12] {
    [
        1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, t[0], t[1], t[2],
    ]
}

/// A closed 20 mm ball.
fn ball() -> Mesh {
    mf_fixtures::scaled(&mf_fixtures::uv_sphere(32, 48), 10.0)
}

fn objects(h: &Harness<'_, MeshFixerApp>) -> Vec<String> {
    h.state()
        .session
        .document()
        .objects()
        .iter()
        .map(|o| o.name.clone())
        .collect()
}

#[test]
fn split_cuts_a_closed_object_in_two_and_refuses_an_open_one() {
    require_gpu!("split_cuts_a_closed_object_in_two_and_refuses_an_open_one");
    let mut h = opened(WINDOW, vec![("ball", ball(), at([0.0, 0.0, 10.0]))]);
    h.get_by_label("Split").click();
    settle(&mut h);
    assert_eq!(h.state().active_tool, "split");
    still(&mut h, "split-before");
    h.get_by_label("Vertical X").click();
    settle(&mut h);
    let n = h.state().split.normal();
    assert!((n.x - 1.0).abs() < 1e-9, "{n:?}");
    // Off the ball's seam of vertices (a plane through a ring of them
    // grazes the surface: refused, "move the plane slightly").
    h.state_mut().split.plane[9] += 1.3;
    settle(&mut h);
    still(&mut h, "split-preview");
    let history = h.state().session.history_labels().len();
    h.get_by_label("Cut in two").click();
    wait_until(&mut h, "the split", |a| {
        a.busy.is_none() && a.session.history_labels().len() > history
    });
    settle(&mut h);
    let names = objects(&h);
    assert!(names.contains(&"ball (top)".to_owned()), "{names:?}");
    assert!(names.contains(&"ball (bottom)".to_owned()), "{names:?}");
    still(&mut h, "split-after");

    // An open model: the refusal, and the way on.
    let mut h = opened(
        WINDOW,
        vec![("plate", mf_fixtures::plate_with_defects(), at([0.0; 3]))],
    );
    h.get_by_label("Split").click();
    settle(&mut h);
    h.get_by_label("Repair holes first");
    still(&mut h, "split-refusal");
    h.get_by_label("Repair holes first").click();
    settle(&mut h);
    assert_eq!(h.state().active_tool, "repair");
}

#[test]
fn combine_subtracts_the_tool_and_shows_where_they_overlap() {
    require_gpu!("combine_subtracts_the_tool_and_shows_where_they_overlap");
    let mut h = opened(
        WINDOW,
        vec![
            ("block", mf_fixtures::cube(20.0), at([0.0; 3])),
            ("peg", ball(), at([20.0, 10.0, 20.0])),
        ],
    );
    h.get_by_label("Combine").click();
    settle(&mut h);
    assert_eq!(h.state().active_tool, "combine");
    wait_until(&mut h, "the overlap", |a| a.combine.tool.is_some());
    // The overlap is found on a background thread.
    let start = Instant::now();
    while h.query_by_label_contains("overlap region").is_none() {
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "no overlap shown"
        );
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    settle(&mut h);
    h.get_by_label("Subtract").click();
    settle(&mut h);
    still(&mut h, "combine-preview");
    let history = h.state().session.history_labels().len();
    h.get_by_label("Combine objects").click();
    wait_until(&mut h, "the boolean", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    assert_eq!(objects(&h), ["block"], "the tool is used up");
    still(&mut h, "combine-after");
}

/// Owner report: the tool stays even with "Keep tool" unchecked. Checked,
/// the tool must survive the union (as the checkbox promises); unchecked
/// again right after, on the next combine, it must actually be removed --
/// not stuck `true` from the previous run, and not merely reported
/// removed while the object lingers in the document.
#[test]
fn unchecking_keep_tool_after_checking_it_removes_the_tool_object() {
    require_gpu!("unchecking_keep_tool_after_checking_it_removes_the_tool_object");
    // "block" and "peg" overlap (as in the test above); combine them
    // twice in a row, same pair both times.
    let mut h = opened(
        WINDOW,
        vec![
            ("block", mf_fixtures::cube(20.0), at([0.0; 3])),
            ("peg", ball(), at([20.0, 10.0, 20.0])),
        ],
    );
    h.get_by_label("Combine").click();
    settle(&mut h);
    assert_eq!(h.state().active_tool, "combine");
    wait_until(&mut h, "the overlap", |a| a.combine.tool.is_some());
    settle(&mut h);
    // Check "Keep the tool object" and union: the tool must survive.
    h.get_by_label("Keep the tool object").click();
    settle(&mut h);
    assert!(h.state().combine.keep_tool, "the checkbox is now checked");
    let history = h.state().session.history_labels().len();
    h.get_by_label("Combine objects").click();
    wait_until(&mut h, "the first combine", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    assert_eq!(
        objects(&h),
        ["block", "peg"],
        "kept: the tool is still in the document"
    );

    // Open Combine again on the same pair and uncheck "Keep the tool
    // object" before applying.
    h.get_by_label("Combine").click();
    settle(&mut h);
    wait_until(&mut h, "the overlap", |a| a.combine.tool.is_some());
    settle(&mut h);
    assert!(
        h.state().combine.keep_tool,
        "keep_tool carries over from the last time Combine was open"
    );
    h.get_by_label("Keep the tool object").click();
    settle(&mut h);
    assert!(
        !h.state().combine.keep_tool,
        "the checkbox is now unchecked"
    );
    let history = h.state().session.history_labels().len();
    h.get_by_label("Combine objects").click();
    wait_until(&mut h, "the second combine", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    assert_eq!(
        objects(&h),
        ["block"],
        "unchecked: the consumed tool object is actually removed"
    );
}

#[test]
fn a_self_overlapping_target_is_refused_with_make_solid_as_the_way_on() {
    require_gpu!("a_self_overlapping_target_is_refused_with_make_solid_as_the_way_on");
    // Two interpenetrating cubes in one object: it overlaps itself.
    let tangled = mf_fixtures::merge(&[
        mf_fixtures::cube(20.0),
        mf_fixtures::translated(&mf_fixtures::cube(20.0), [10.0, 6.0, 4.0]),
    ]);
    let mut h = opened(
        WINDOW,
        vec![
            ("tangle", tangled, at([0.0; 3])),
            ("cutter", ball(), at([15.0, 10.0, 12.0])),
        ],
    );
    h.get_by_label("Combine").click();
    settle(&mut h);
    h.get_by_label("Subtract").click();
    settle(&mut h);
    h.get_by_label("Combine objects").click();
    wait_until(&mut h, "the refusal", |a| {
        a.busy.is_none() && a.refusal.is_some()
    });
    settle(&mut h);
    let code = h.state().refusal.clone().unwrap().code;
    eprintln!("refused: {code}");
    still(&mut h, "combine-refusal");
    assert_eq!(code, "operand_overlaps_itself");
    // The refusal's next action opens Make solid on the target.
    h.get_by_label("Run Make solid first").click();
    settle(&mut h);
    assert_eq!(h.state().active_tool, "make_solid");
    still(&mut h, "solid-before");
    let history = h.state().session.history_labels().len();
    h.get_by_label("Make it solid").click();
    wait_until(&mut h, "make solid", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    assert!(
        h.state().current_analysis().unwrap().diagnostics.is_closed,
        "the result is one closed solid"
    );
    still(&mut h, "solid-after");
}

/// Owner report: after a split, picking one part (in the Browser or the
/// view) makes that part the target, so Delete removes only it.
#[test]
fn after_a_split_delete_removes_only_the_picked_part() {
    require_gpu!("after_a_split_delete_removes_only_the_picked_part");
    let mut h = opened(WINDOW, vec![("ball", ball(), at([0.0, 0.0, 10.0]))]);
    h.get_by_label("Split").click();
    settle(&mut h);
    h.state_mut().split.plane[11] += 1.3;
    settle(&mut h);
    let history = h.state().session.history_labels().len();
    h.get_by_label("Cut in two").click();
    wait_until(&mut h, "the split", |a| {
        a.busy.is_none() && a.session.history_labels().len() > history
    });
    settle(&mut h);
    h.key_press(egui::Key::Escape);
    settle(&mut h);
    // The Browser row of the bottom part (Split just took the document
    // from one object to two, so the Browser is shown now even though it
    // defaulted to closed on the single-object document a moment ago).
    assert_eq!(
        h.state().browser_open,
        Some(true),
        "the Browser reopens once there's more than one object to pick between"
    );
    h.get_by_role_and_label(egui::accesskit::Role::Button, "ball (bottom)")
        .click();
    settle(&mut h);
    let bottom = h
        .state()
        .session
        .document()
        .objects()
        .iter()
        .find(|o| o.name == "ball (bottom)")
        .unwrap()
        .id;
    assert_eq!(
        h.state().active,
        Some(bottom),
        "the picked part is the target"
    );
    h.key_press(egui::Key::Delete);
    wait_until(&mut h, "the delete", |a| {
        a.busy.is_none() && a.session.document().objects().len() == 1
    });
    settle(&mut h);
    assert_eq!(objects(&h), ["ball (top)"]);

    // Undo; then pick the bottom part in the view instead.
    h.get_by_label("Undo").click();
    wait_until(&mut h, "the undo", |a| {
        a.busy.is_none() && a.session.document().objects().len() == 2
    });
    settle(&mut h);
    let top = h
        .state()
        .session
        .document()
        .objects()
        .iter()
        .find(|o| o.name == "ball (top)")
        .unwrap()
        .id;
    h.state_mut().set_active(top);
    settle(&mut h);
    let at_bottom = h.state().screen_point([0.0, 0.0, 3.0]).unwrap();
    h.event(egui::Event::PointerMoved(at_bottom));
    h.step();
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos: at_bottom,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
    }
    wait_until(&mut h, "the pick", |a| a.active == Some(bottom));
    settle(&mut h);
    h.key_press(egui::Key::Delete);
    wait_until(&mut h, "the delete", |a| {
        a.busy.is_none() && a.session.document().objects().len() == 1
    });
    assert_eq!(objects(&h), ["ball (top)"]);
}

/// A second object in the scene, the Split tool left open (no Escape),
/// and a view click landing on the part the split *didn't* auto-activate
/// ("top" keeps the original id; "bottom" is the new, auto-activated
/// part; see `mf_operations::split`): Delete right after that click must
/// still act on the part the click picked, not a stale target.
#[test]
fn delete_after_a_view_click_targets_the_clicked_part_not_a_stale_one() {
    require_gpu!("delete_after_a_view_click_targets_the_clicked_part_not_a_stale_one");
    let mut h = opened(
        WINDOW,
        vec![
            ("cube", mf_fixtures::cube(5.0), at([20.0, 0.0, 2.5])),
            ("ball", ball(), at([0.0, 0.0, 10.0])),
        ],
    );
    let ball_id = h
        .state()
        .session
        .document()
        .objects()
        .iter()
        .find(|o| o.name == "ball")
        .unwrap()
        .id;
    h.state_mut().set_active(ball_id);
    settle(&mut h);
    h.get_by_label("Split").click();
    settle(&mut h);
    h.state_mut().split.plane[11] += 1.3;
    settle(&mut h);
    let history = h.state().session.history_labels().len();
    h.get_by_label("Cut in two").click();
    wait_until(&mut h, "the split", |a| {
        a.busy.is_none() && a.session.history_labels().len() > history
    });
    settle(&mut h);
    // No Escape this time: click in the view, on the top half, with the
    // Split tool still open, then Delete, all in quick succession.
    let at_top = h.state().screen_point([0.0, 0.0, 17.0]).unwrap();
    h.event(egui::Event::PointerMoved(at_top));
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos: at_top,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
    h.event(egui::Event::Key {
        key: egui::Key::Delete,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.event(egui::Event::Key {
        key: egui::Key::Delete,
        physical_key: None,
        pressed: false,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    wait_until(&mut h, "the delete", |a| {
        a.busy.is_none() && a.session.document().objects().len() == 2
    });
    settle(&mut h);
    assert_eq!(
        objects(&h),
        ["cube", "ball (bottom)"],
        "the clicked part (top) is removed, not a stale target"
    );
}

/// Owner report, root cause: clicking an object's own row in the Browser
/// only called `set_active`, which leaves a sub-selection alone when it
/// already belongs to that object (only a *different* object's selection
/// is cleared). After a split, re-picking the already-active part with a
/// leftover sub-selection (e.g. a boundary loop picked earlier) silently
/// blocked Delete, with nothing in the row to say why. The object row is
/// a whole-object pick, so it must clear any selection, like "Model"
/// does.
#[test]
fn clicking_an_objects_own_row_clears_a_leftover_sub_selection_so_delete_works() {
    require_gpu!("clicking_an_objects_own_row_clears_a_leftover_sub_selection_so_delete_works");
    let mut h = opened(WINDOW, vec![("ball", ball(), at([0.0, 0.0, 10.0]))]);
    h.get_by_label("Split").click();
    settle(&mut h);
    h.state_mut().split.plane[11] += 1.3;
    settle(&mut h);
    let history = h.state().session.history_labels().len();
    h.get_by_label("Cut in two").click();
    wait_until(&mut h, "the split", |a| {
        a.busy.is_none() && a.session.history_labels().len() > history
    });
    settle(&mut h);
    h.key_press(egui::Key::Escape);
    settle(&mut h);
    let bottom = h
        .state()
        .session
        .document()
        .objects()
        .iter()
        .find(|o| o.name == "ball (bottom)")
        .unwrap()
        .id;
    h.state_mut().set_active(bottom);
    // A leftover sub-selection on the already-active part (as a boundary
    // loop pick, or any other "smaller than the object" selection, would
    // leave behind): `current_selection()` must be `None` after clicking
    // the object's own row again, or Delete silently refuses.
    h.state_mut().selection = Some(Selection {
        object: bottom,
        revision: h.state().session.current(),
        kind: SelectionKind::BoundaryLoop,
        ids: vec![0],
    });
    settle(&mut h);
    assert!(
        h.state().current_selection().is_some(),
        "the sub-selection is in place before the row click"
    );
    h.get_by_role_and_label(egui::accesskit::Role::Button, "ball (bottom)")
        .click();
    settle(&mut h);
    assert_eq!(h.state().active, Some(bottom));
    assert!(
        h.state().current_selection().is_none(),
        "the object row's click clears a leftover sub-selection"
    );
    h.key_press(egui::Key::Delete);
    wait_until(&mut h, "the delete", |a| {
        a.busy.is_none() && a.session.document().objects().len() == 1
    });
    assert_eq!(objects(&h), ["ball (top)"]);
}
