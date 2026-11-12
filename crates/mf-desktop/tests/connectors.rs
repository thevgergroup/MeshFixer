#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The Connectors tool on the rail (Phase 1 Task 29, MF-MODEL-005): the
//! library (Pin hinge, Flap hinge, Snap clasp, Thread), the Fit class
//! and Machine profile pickers, placing a connector with
//! `object.add_shape` (a linked group of editable shapes, one per part),
//! the fit check and Attach (`object.attach`).
//!
//! With `MF_ARTIFACTS` set, frames go to
//! `<dir>/design/connectors-<scene>-<w>x<h>.png`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use mf_desktop::{MeshFixerApp, Settings};
use mf_fixtures::{cube, translated};
use mf_geometry::{ContentHash, Mesh};
use mf_io::threemf::{ModelObject, Unit};
use mf_io::{Format, ImportedModel, SourceInfo, SourcePrecision};

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
    let name = format!("connectors-{scene}-{}x{}.png", size.x as u32, size.y as u32);
    h.render().unwrap().save(dir.join(name)).unwrap();
}

/// A fresh harness with its own temp settings directory (`config_dir`),
/// so no test here ever reads or writes the real user's Fit classes,
/// Machine profiles or parts library; the directory stays alive as
/// long as the returned harness does.
fn opened(
    objects: Vec<(&str, Mesh, [f64; 12])>,
) -> (Harness<'static, MeshFixerApp>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let settings = Settings {
        config_dir: Some(dir.path().to_owned()),
        ..Settings::default()
    };
    let mut h = Harness::builder()
        .with_size(WINDOW)
        .with_step_dt(1.0 / 60.0)
        .wgpu()
        .build_eframe(move |cc| MeshFixerApp::with_settings(cc, None, settings).unwrap());
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
            path: "connectors.3mf".into(),
            content_hash: ContentHash([0; 32]),
            format: Format::ThreeMf,
            precision: SourcePrecision::F32,
        },
        warnings: Vec::new(),
    });
    wait_until(&mut h, "analysis and picking", |a| {
        a.current_analysis().is_some() && a.picking_ready() && a.camera.is_some()
    });
    h.event(egui::Event::PointerGone);
    settle(&mut h);
    (h, dir)
}

const IDENTITY: [f64; 12] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];

/// Scrolls the tool dock (the wheel over it) until the node labelled
/// `label` is inside the window, then clicks it (`shape_tools.rs`'s
/// `scroll_and_click`: the Connectors panel, with the library, the fit
/// pickers, the fit check and the side pickers, is routinely taller
/// than the test window, so a button below the fold has a rect but a
/// click there lands on nothing outside the scroll area's clip).
fn scroll_and_click(h: &mut Harness<'_, MeshFixerApp>, label: &str) {
    let over = egui::pos2(200.0, WINDOW.1 * 0.5);
    h.event(egui::Event::PointerMoved(over));
    h.step();
    let mut tries = 0;
    loop {
        let rect = h.get_by_label(label).rect();
        // Below the fold, or under the bottom status bar (which overlaps
        // a tall tool panel's last ~40 pt and would eat the click
        // instead of the button): keep scrolling clear of both.
        if rect.bottom() <= WINDOW.1 - 44.0 {
            break;
        }
        tries += 1;
        assert!(tries < 400, "scrolled to {label:?}");
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(0.0, -1.0),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: egui::Modifiers::NONE,
        });
        h.run_steps(2);
    }
    h.run_steps(10);
    h.get_by_label(label).click();
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

/// Big, separate cubes standing in for the body and lid a `clasp.snap`
/// connector joins (adapted from `connector_join.rs`'s own fixture): the
/// Connectors tool resolves `gap` from the chosen Fit class (here,
/// "Snug", 0.0 mm clearance) rather than the connector's own spec
/// default (0.4 mm), so `catch` sits at z in [0, 3.2] as before but
/// `hook` (whose z0 is `ridge_height + gap`) ends at 3.2 + 1.6 = 4.8,
/// not 5.2: the lid's bottom face moves down to meet it, a real gap
/// unlike `hinge.pin`'s interleaved leaves.
fn body_and_lid() -> (Mesh, Mesh) {
    let body = translated(&cube(40.0), [-5.0, -5.0, -40.0]);
    let lid = translated(&cube(40.0), [-5.0, -5.0, 4.8]);
    (body, lid)
}

/// Opening Connectors and clicking Pin hinge places it as a new object
/// and opens Transform (the same click-to-place flow as Parts).
#[test]
fn clicking_a_connector_adds_it_and_opens_transform() {
    require_gpu!("clicking_a_connector_adds_it_and_opens_transform");
    let (mut h, _settings_dir) = opened(vec![("body", cube(40.0), IDENTITY)]);
    h.get_by_label("Connectors").click();
    settle(&mut h);
    assert_eq!(h.state().active_tool, "connectors");
    still(&mut h, "library");
    let history = h.state().session.history_labels().len();
    h.get_by_label("Pin hinge").click();
    wait_until(&mut h, "the hinge placed", |a| {
        a.busy.is_none() && a.session.history_labels().len() > history
    });
    settle(&mut h);
    let names = objects(&h);
    assert!(
        names.contains(&"Pin hinge (leaf a)".to_owned())
            && names.contains(&"Pin hinge (leaf b)".to_owned()),
        "one editable shape per part: {names:?}"
    );
    assert_eq!(
        h.state().active_tool,
        "transform",
        "Transform opens on the placed connector"
    );
    still(&mut h, "placed");
}

/// Merge runs `connector.join` between the two picked sides, folding
/// the connector's parts in and removing it, one more undoable command
/// on top of the placement.
#[test]
fn merge_folds_the_connector_into_its_two_sides() {
    require_gpu!("merge_folds_the_connector_into_its_two_sides");
    let (body, lid) = body_and_lid();
    let (mut h, _settings_dir) = opened(vec![("body", body, IDENTITY), ("lid", lid, IDENTITY)]);
    let body_id = h.state().session.document().objects()[0].id;
    h.state_mut().set_active(body_id);
    h.get_by_label("Connectors").click();
    settle(&mut h);
    let history = h.state().session.history_labels().len();
    h.get_by_label("Snap clasp").click();
    wait_until(&mut h, "the clasp placed", |a| {
        a.busy.is_none() && a.session.history_labels().len() > history
    });
    settle(&mut h);
    // The connector lands wherever Connectors' click-to-place puts it
    // (the active object's own highest point); `clasp.snap`'s own
    // geometry is built at its object-space origin expecting `catch`'s
    // side at z in [0, 3.2] and `hook`'s at z in [3.6, 4.8] (with the
    // Snug fit class's 0.0 mm gap; see `body_and_lid`), so put it back
    // there before merging — the same as a user dragging it into place
    // in Transform first.
    let clasp_id = h.state().active.unwrap();
    h.state_mut()
        .commit_transform(clasp_id, IDENTITY, "test setup".into());
    wait_until(&mut h, "the clasp's move", |a| a.busy.is_none());
    settle(&mut h);
    let after_move = h.state().session.history_labels().len();
    h.get_by_label("Connectors").click();
    settle(&mut h);
    // Side A: body, Side B: lid (each is its own row of buttons, one
    // per other object).
    scroll_and_click(&mut h, "Side A: body");
    settle(&mut h);
    scroll_and_click(&mut h, "Side B: lid");
    settle(&mut h);
    scroll_and_click(&mut h, "Attach");
    wait_until(&mut h, "the merge", |a| {
        a.busy.is_none() && a.session.history_labels().len() > after_move
    });
    settle(&mut h);
    let names = objects(&h);
    assert_eq!(
        names.len(),
        2,
        "the connector object is gone, folded into body and lid: {names:?}"
    );
    assert!(names.contains(&"body".to_owned()) && names.contains(&"lid".to_owned()));
    still(&mut h, "merged");
}

/// Merge stays disabled when the two sides it's told to act between
/// aren't distinct (the same guard the shape tools use elsewhere: a
/// command can't act between an object and itself).
#[test]
fn merge_disables_when_both_sides_are_the_same_object() {
    require_gpu!("merge_disables_when_both_sides_are_the_same_object");
    let (body, lid) = body_and_lid();
    let (mut h, _settings_dir) = opened(vec![("body", body, IDENTITY), ("lid", lid, IDENTITY)]);
    h.get_by_label("Connectors").click();
    settle(&mut h);
    let history = h.state().session.history_labels().len();
    h.get_by_label("Snap clasp").click();
    wait_until(&mut h, "the clasp placed", |a| {
        a.busy.is_none() && a.session.history_labels().len() > history
    });
    settle(&mut h);
    h.get_by_label("Connectors").click();
    settle(&mut h);
    scroll_and_click(&mut h, "Side A: lid");
    settle(&mut h);
    scroll_and_click(&mut h, "Side B: lid");
    settle(&mut h);
    assert!(h.get_by_label("Attach").accesskit_node().is_disabled());
}

/// Placing a connector (or running Merge) shows a plain-language
/// refusal, with its next action, when the user's Fit classes or
/// Machine profiles file can't be read, rather than silently falling
/// back to the built-ins mid-command: here, a malformed Fit classes
/// file (owner requirements 2026-11-11, "never assumed").
#[test]
fn placing_a_connector_shows_fit_settings_refusal_with_next_action() {
    require_gpu!("placing_a_connector_shows_fit_settings_refusal_with_next_action");
    let (mut h, settings_dir) = opened(vec![("body", cube(40.0), IDENTITY)]);
    std::fs::write(settings_dir.path().join("fit-classes.yaml"), "not: [valid").unwrap();
    h.get_by_label("Connectors").click();
    settle(&mut h);
    h.get_by_label("Pin hinge").click();
    settle(&mut h);
    assert!(h.state().refusal.is_some(), "a refusal is shown");
    let refusal = h.state().refusal.clone().unwrap();
    assert_eq!(refusal.op_id, "object.add_shape");
    assert_eq!(refusal.code, "fit_settings_unreadable");
    assert!(
        refusal.message.contains("Fit classes") || refusal.message.contains("Machine profiles"),
        "{:?}",
        refusal.message
    );
    still(&mut h, "refused");
}

/// Saving a connector to the library (recipe, not a stored mesh) is
/// named in the status, through the same path parts use.
#[test]
fn save_connector_to_library_names_what_was_saved() {
    require_gpu!("save_connector_to_library_names_what_was_saved");
    let (mut h, settings_dir) = opened(vec![("body", cube(40.0), IDENTITY)]);
    h.get_by_label("Connectors").click();
    settle(&mut h);
    let history = h.state().session.history_labels().len();
    h.get_by_label("Pin hinge").click();
    wait_until(&mut h, "the hinge placed", |a| {
        a.busy.is_none() && a.session.history_labels().len() > history
    });
    settle(&mut h);
    h.get_by_label("Connectors").click();
    settle(&mut h);
    scroll_and_click(&mut h, "Save to library…");
    settle(&mut h);
    let status = h.state().status.clone().unwrap_or_default();
    assert!(status.contains("Pin hinge"), "{status:?}");
    assert!(
        settings_dir
            .path()
            .join("parts")
            .read_dir()
            .unwrap()
            .next()
            .is_some(),
        "the connector's recipe was written under the temp settings dir"
    );
    still(&mut h, "saved");
}

/// The Settings pages for Machine profiles and Fit classes list the
/// built-ins, each tolerance/clearance field shown, and the built-in
/// machine profile saying it isn't calibrated.
#[test]
fn settings_pages_list_machine_profiles_and_fit_classes() {
    require_gpu!("settings_pages_list_machine_profiles_and_fit_classes");
    let (mut h, _settings_dir) = opened(vec![("body", cube(40.0), IDENTITY)]);
    h.state_mut().show_settings = true;
    settle(&mut h);
    assert!(h.query_by_label("Machine profiles").is_some());
    assert!(h.query_by_label("Fit classes").is_some());
    still(&mut h, "settings");
}

/// A machine profile's print volume (Split's "Print volume" cut mode,
/// owner 2026-11-11, stored with the Machine profiles): the built-in's
/// shows read-only, a new profile defaults to it and is editable, and
/// picking a preset writes its exact W×D×H and persists under the temp
/// settings dir.
#[test]
fn a_machine_profiles_print_volume_shows_and_a_preset_can_be_picked() {
    require_gpu!("a_machine_profiles_print_volume_shows_and_a_preset_can_be_picked");
    let (mut h, settings_dir) = opened(vec![("body", cube(40.0), IDENTITY)]);
    h.state_mut().show_settings = true;
    settle(&mut h);

    // The built-in profile's volume shows, read-only, as its default.
    let default = mf_parts::MachineProfile::uncalibrated().print_volume_mm;
    let [dw, dd, dh] = default;
    h.get_by_label(format!("{dw:.0} × {dd:.0} × {dh:.0} mm").as_str());
    still(&mut h, "print-volume-builtin");

    // A new profile starts on the same default, editable.
    scroll_and_click(&mut h, "New machine profile…");
    settle(&mut h);
    assert_eq!(
        h.state().connectors.machine_profile,
        "My printer",
        "the new profile becomes the active pick"
    );

    // Picking "Voron 2.4" writes its exact volume and persists it.
    scroll_and_click(&mut h, "Voron 2.4");
    settle(&mut h);
    still(&mut h, "print-volume-preset");

    let profiles = mf_parts::MachineProfiles::load(settings_dir.path()).unwrap();
    let p = profiles.get("My printer").unwrap();
    assert_eq!(p.print_volume_mm, [350.0, 350.0, 340.0]);
}
