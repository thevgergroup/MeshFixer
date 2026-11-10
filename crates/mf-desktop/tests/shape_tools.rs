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
                metadata: Default::default(),
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

/// The Browser row of `name` (an object row is a button).
fn row<'a>(h: &'a Harness<'_, MeshFixerApp>, name: &'a str) -> egui_kittest::Node<'a> {
    h.get_by_role_and_label(egui::accesskit::Role::Button, name)
}

/// The widest node labelled `label`: the panel's own button where the
/// rail has an icon of the same name ("Combine").
fn panel_button<'a>(h: &'a Harness<'_, MeshFixerApp>, label: &'a str) -> egui_kittest::Node<'a> {
    h.query_all_by_label(label)
        .max_by(|a, b| a.rect().width().total_cmp(&b.rect().width()))
        .unwrap_or_else(|| panic!("no {label:?}"))
}

/// Click `names`' Browser rows: the first plainly, the rest with Shift
/// (selected together).
fn select_together(h: &mut Harness<'_, MeshFixerApp>, names: &[&str]) {
    for (k, name) in names.iter().enumerate() {
        if k == 0 {
            row(h, name).click();
        } else {
            row(h, name).click_modifiers(egui::Modifiers::SHIFT);
        }
        settle(h);
    }
}

fn id_of(h: &Harness<'_, MeshFixerApp>, name: &str) -> mf_command::ObjectId {
    h.state()
        .session
        .document()
        .objects()
        .iter()
        .find(|o| o.name == name)
        .unwrap_or_else(|| panic!("no object {name:?}"))
        .id
}

fn is_hole(h: &Harness<'_, MeshFixerApp>, name: &str) -> bool {
    h.state().is_hole(id_of(h, name))
}

/// A block, a post on it and a peg through its side.
fn block_post_peg() -> Vec<(&'static str, Mesh, [f64; 12])> {
    vec![
        ("block", mf_fixtures::cube(20.0), at([0.0; 3])),
        ("post", mf_fixtures::cube(8.0), at([6.0, 6.0, 16.0])),
        ("peg", ball(), at([20.0, 10.0, 10.0])),
    ]
}

/// H toggles the role (one undoable step); the Browser row gets the hole
/// icon and says "(hole)"; the view draws it hatched; and the role is
/// saved in the 3MF as object metadata.
#[test]
fn h_toggles_a_hole_which_draws_hatched_and_is_saved() {
    require_gpu!("h_toggles_a_hole_which_draws_hatched_and_is_saved");
    let mut h = opened(WINDOW, block_post_peg());
    row(&h, "peg").click();
    settle(&mut h);
    let centre = h.state().screen_point([20.0, 10.0, 10.0]).unwrap();
    let before = h.render().unwrap();
    h.key_press(egui::Key::H);
    settle(&mut h);
    assert!(is_hole(&h, "peg"));
    // The row says so (its accessible label, with the hole icon).
    row(&h, "peg (hole)");
    still(&mut h, "role-hole");
    let after = h.render().unwrap();
    // The peg's pixels: plain before, a hatch after (brightness crosses
    // its mean many times across 40 points of the ball).
    let ppp = h.ctx.pixels_per_point();
    let y = (centre.y * ppp) as usize;
    let x0 = ((centre.x - 20.0) * ppp) as usize;
    let n = (40.0 * ppp) as usize;
    let crossings = |raw: &[u8], width: usize| {
        let values: Vec<f64> = (x0..x0 + n)
            .map(|x| {
                let p = &raw[(y * width + x) * 4..][..3];
                0.299 * p[0] as f64 + 0.587 * p[1] as f64 + 0.114 * p[2] as f64
            })
            .collect();
        let mean = values.iter().sum::<f64>() / values.len() as f64;
        values
            .windows(2)
            .filter(|w| (w[0] - mean).signum() != (w[1] - mean).signum())
            .count()
    };
    let (plain, hatched) = (
        crossings(before.as_raw(), before.width() as usize),
        crossings(after.as_raw(), after.width() as usize),
    );
    assert!(
        hatched >= 6 && hatched > 2 * plain,
        "hatch: {plain} crossings before, {hatched} after"
    );
    // One undo step back to a solid; H again makes it a hole.
    h.get_by_label("Undo").click();
    settle(&mut h);
    assert!(!is_hole(&h, "peg"));
    row(&h, "peg").click();
    settle(&mut h);
    h.key_press(egui::Key::H);
    settle(&mut h);
    assert!(is_hole(&h, "peg"));
    let model = h.state().document_model().unwrap();
    let peg = model.objects.iter().find(|o| o.name == "peg").unwrap();
    assert_eq!(
        peg.metadata.get("meshfixer:role").map(String::as_str),
        Some("hole")
    );
    let block = model.objects.iter().find(|o| o.name == "block").unwrap();
    assert!(block.metadata.is_empty());
}

/// The Browser row's right-click menu switches the role too, and with
/// several objects selected together H switches them all in one step.
#[test]
fn the_browser_menu_and_h_switch_roles_of_the_selected_objects() {
    require_gpu!("the_browser_menu_and_h_switch_roles_of_the_selected_objects");
    let mut h = opened(WINDOW, block_post_peg());
    row(&h, "peg").click_secondary();
    settle(&mut h);
    h.get_by_label("Make it a hole").click();
    settle(&mut h);
    assert!(is_hole(&h, "peg"));
    select_together(&mut h, &["block", "post"]);
    assert_eq!(
        h.state().selected_objects(),
        [id_of(&h, "block"), id_of(&h, "post")]
    );
    let steps = h.state().session.history_labels().len();
    h.key_press(egui::Key::H);
    settle(&mut h);
    assert!(is_hole(&h, "block") && is_hole(&h, "post"));
    assert_eq!(h.state().session.history_labels().len(), steps + 1);
    // Shift-click again takes one out of the selection.
    row(&h, "post (hole)").click_modifiers(egui::Modifiers::SHIFT);
    settle(&mut h);
    assert_eq!(h.state().selected_objects(), [id_of(&h, "block")]);
}

/// Click `world` in the view, with `modifiers`.
fn click_view(h: &mut Harness<'_, MeshFixerApp>, world: [f64; 3], modifiers: egui::Modifiers) {
    let at = h.state().screen_point(world).unwrap();
    h.event(egui::Event::ModifiersChanged(modifiers));
    h.event(egui::Event::PointerMoved(at));
    h.step();
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers,
        });
        h.step();
    }
    h.event(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    settle(h);
}

/// Shift-click in the view selects objects together too (and a plain
/// click selects just one again).
#[test]
fn shift_click_in_the_view_selects_objects_together() {
    require_gpu!("shift_click_in_the_view_selects_objects_together");
    let mut h = opened(WINDOW, block_post_peg());
    h.get_by_label("Combine").click();
    settle(&mut h);
    click_view(&mut h, [10.0, 2.0, 10.0], egui::Modifiers::NONE);
    wait_until(&mut h, "the block", |a| {
        a.active == Some(id_of_app(a, "block"))
    });
    click_view(&mut h, [10.0, 10.0, 24.0], egui::Modifiers::SHIFT);
    wait_until(&mut h, "block and post", |a| {
        a.selected_objects().len() == 2
    });
    assert_eq!(
        h.state().selected_objects(),
        [id_of(&h, "block"), id_of(&h, "post")]
    );
    h.get_by_label("Joins 2 solids and cuts 0 holes");
    still(&mut h, "combine-view-picked");
    click_view(&mut h, [10.0, 10.0, 24.0], egui::Modifiers::NONE);
    wait_until(&mut h, "just the post", |a| a.selected_objects().len() == 1);
    assert_eq!(h.state().selected_objects(), [id_of(&h, "post")]);
}

fn id_of_app(a: &MeshFixerApp, name: &str) -> mf_command::ObjectId {
    a.session
        .document()
        .objects()
        .iter()
        .find(|o| o.name == name)
        .unwrap()
        .id
}

/// Combine on the objects selected together: the panel says what it will
/// do and lists each object with its role; one Combine joins the solids
/// and cuts the hole, as one command (one undo restores all three).
#[test]
fn combine_joins_the_solids_and_cuts_the_holes_selected() {
    require_gpu!("combine_joins_the_solids_and_cuts_the_holes_selected");
    let mut h = opened(WINDOW, block_post_peg());
    row(&h, "peg").click();
    settle(&mut h);
    h.key_press(egui::Key::H);
    settle(&mut h);
    select_together(&mut h, &["block", "post", "peg (hole)"]);
    h.get_by_label("Combine").click();
    settle(&mut h);
    assert_eq!(h.state().active_tool, "combine");
    h.get_by_label("Joins 2 solids and cuts 1 hole");
    h.get_by_label("block · Solid");
    h.get_by_label("post · Solid");
    h.get_by_label("peg · Hole");
    // No target/tool picker any more; Intersect sits under More.
    assert!(h.query_by_label("Tool").is_none());
    assert!(h.query_by_label("Intersect").is_none());
    still(&mut h, "combine-preview");
    let history = h.state().session.history_labels().len();
    panel_button(&h, "Combine").click();
    wait_until(&mut h, "the combine", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    assert_eq!(objects(&h), ["block"], "post joined, peg cut and used up");
    assert_eq!(h.state().session.history_labels().len(), history + 1);
    still(&mut h, "combine-after");
    h.get_by_label("Undo").click();
    settle(&mut h);
    assert_eq!(objects(&h), ["block", "post", "peg"]);
    assert!(is_hole(&h, "peg"), "undo restores the hole with its role");
}

/// More ▸ Keep the hole objects keeps the holes; More ▸ Intersect works
/// on exactly two selected objects.
#[test]
fn more_keeps_holes_and_intersects_two() {
    require_gpu!("more_keeps_holes_and_intersects_two");
    let mut h = opened(WINDOW, block_post_peg());
    row(&h, "peg").click();
    settle(&mut h);
    h.key_press(egui::Key::H);
    settle(&mut h);
    select_together(&mut h, &["block", "peg (hole)"]);
    h.get_by_label("Combine").click();
    settle(&mut h);
    h.get_by_label("More").click();
    settle(&mut h);
    h.get_by_label("Keep the hole objects").click();
    settle(&mut h);
    assert!(h.state().combine.keep_holes);
    still(&mut h, "combine-more");
    let history = h.state().session.history_labels().len();
    panel_button(&h, "Combine").click();
    wait_until(&mut h, "the combine", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    assert_eq!(objects(&h), ["block", "post", "peg"], "the hole is kept");
    // Intersect the block and the post.
    select_together(&mut h, &["block", "post"]);
    let history = h.state().session.history_labels().len();
    h.get_by_label("Intersect").click();
    wait_until(&mut h, "the intersect", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    assert_eq!(objects(&h), ["block", "peg"]);
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
    row(&h, "cutter").click();
    settle(&mut h);
    h.key_press(egui::Key::H);
    settle(&mut h);
    select_together(&mut h, &["tangle", "cutter (hole)"]);
    h.get_by_label("Combine").click();
    settle(&mut h);
    panel_button(&h, "Combine").click();
    wait_until(&mut h, "the refusal", |a| {
        a.busy.is_none() && a.refusal.is_some()
    });
    settle(&mut h);
    let refusal = h.state().refusal.clone().unwrap();
    still(&mut h, "combine-refusal");
    assert_eq!(refusal.code, "operand_overlaps_itself");
    assert!(refusal.message.contains("'tangle'"), "{}", refusal.message);
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

/// Split acts on every selected object, as one command.
#[test]
fn split_cuts_every_selected_object() {
    require_gpu!("split_cuts_every_selected_object");
    let mut h = opened(
        WINDOW,
        vec![
            ("left", ball(), at([0.0, 0.0, 10.0])),
            ("right", ball(), at([40.0, 0.0, 10.0])),
        ],
    );
    select_together(&mut h, &["left", "right"]);
    h.get_by_label("Split").click();
    settle(&mut h);
    h.get_by_label("Cuts 2 selected objects");
    h.state_mut().split.plane[11] = 11.3;
    settle(&mut h);
    let history = h.state().session.history_labels().len();
    h.get_by_label("Cut in two").click();
    wait_until(&mut h, "the split", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    let mut names = objects(&h);
    names.sort();
    assert_eq!(
        names,
        [
            "left (bottom)",
            "left (top)",
            "right (bottom)",
            "right (top)"
        ]
    );
    assert_eq!(h.state().session.history_labels().len(), history + 1);
    still(&mut h, "split-many");
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

// ---------------------------------------------------------------------
// Visible Solid/Hole controls (owner feedback: H and the right-click menu
// alone are hard to discover). All three switches below go through
// `MeshFixerApp::toggle_role`, so each is one undo step, same as H.
// ---------------------------------------------------------------------

/// The summary panel's segmented Solid/Hole control (no tool open, an
/// object selected): clicking Hole switches the active object, one undo
/// restores it, and the control's tooltip names the shortcut.
#[test]
fn the_selection_card_segmented_control_switches_the_role_in_one_undo_step() {
    require_gpu!("the_selection_card_segmented_control_switches_the_role_in_one_undo_step");
    let mut h = opened(WINDOW, block_post_peg());
    row(&h, "peg").click();
    settle(&mut h);
    assert_eq!(h.state().active_tool, "select");
    h.get_by_label("Solid");
    still(&mut h, "selection-card-solid");
    let history = h.state().session.history_labels().len();
    h.get_by_label("Hole").click();
    settle(&mut h);
    assert!(is_hole(&h, "peg"));
    assert_eq!(h.state().session.history_labels().len(), history + 1);
    still(&mut h, "selection-card-hole");
    h.get_by_label("Undo").click();
    settle(&mut h);
    assert!(!is_hole(&h, "peg"));
    // Clicking the segment already current is a no-op (no extra undo step).
    let history = h.state().session.history_labels().len();
    h.get_by_label("Solid").click();
    settle(&mut h);
    assert_eq!(h.state().session.history_labels().len(), history);
}

/// Combine's per-object row has its own Solid/Hole toggle: clicking it
/// switches just that object (one undo step), and the panel's role labels
/// update to match.
#[test]
fn combines_row_toggle_switches_one_objects_role() {
    require_gpu!("combines_row_toggle_switches_one_objects_role");
    let mut h = opened(WINDOW, block_post_peg());
    select_together(&mut h, &["block", "post"]);
    h.get_by_label("Combine").click();
    settle(&mut h);
    h.get_by_label("block · Solid");
    h.get_by_label("post · Solid");
    still(&mut h, "combine-row-toggle-before");
    let history = h.state().session.history_labels().len();
    h.get_by_label("post: Make it a hole").click();
    settle(&mut h);
    assert!(is_hole(&h, "post"));
    assert_eq!(h.state().session.history_labels().len(), history + 1);
    h.get_by_label("block · Solid");
    h.get_by_label("post · Hole");
    still(&mut h, "combine-row-toggle-after");
    h.get_by_label("Undo").click();
    settle(&mut h);
    assert!(!is_hole(&h, "post"));
}

/// A Browser row's Solid/Hole toggle only shows on hover, so rows stay
/// clean otherwise; one click switches just that row's object, as one
/// undo step.
#[test]
fn a_browser_rows_toggle_only_shows_on_hover_and_switches_its_role() {
    require_gpu!("a_browser_rows_toggle_only_shows_on_hover_and_switches_its_role");
    let mut h = opened(WINDOW, block_post_peg());
    assert!(
        h.query_by_label("peg: Make it a hole").is_none(),
        "hidden until the row is hovered"
    );
    row(&h, "peg").hover();
    settle(&mut h);
    still(&mut h, "browser-row-hover");
    let history = h.state().session.history_labels().len();
    h.get_by_label("peg: Make it a hole").click();
    settle(&mut h);
    assert!(is_hole(&h, "peg"));
    assert_eq!(h.state().session.history_labels().len(), history + 1);
    row(&h, "peg (hole)").hover();
    settle(&mut h);
    h.get_by_label("peg: Make it a solid").click();
    settle(&mut h);
    assert!(!is_hole(&h, "peg"));
}
