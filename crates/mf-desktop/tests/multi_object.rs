#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Transform-aware rendering and picking (MF-ISSUE-042): a document with
//! several objects draws and picks every one at its own 3MF transform.

use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use mf_desktop::MeshFixerApp;
use mf_fixtures::cube;
use mf_io::threemf::{IDENTITY, Model, ModelObject, Unit};

fn wait_until(h: &mut Harness<'_, MeshFixerApp>, what: &str, cond: impl Fn(&MeshFixerApp) -> bool) {
    let start = Instant::now();
    while !cond(h.state()) {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "timed out waiting for: {what}"
        );
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    h.run_steps(3);
}

/// A 3MF with two unit cubes translated to (-3, 0, 0) and (3, 0, 0), far
/// enough apart that their screen projections don't overlap.
fn two_translated_cubes(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("two_cubes.3mf");
    let translate = |x: f64| {
        let mut t = IDENTITY;
        t[9] = x;
        t
    };
    let model = Model {
        unit: Unit::Millimeter,
        objects: vec![
            ModelObject {
                name: "left".into(),
                mesh: cube(1.0),
                transform: translate(-3.0),
            },
            ModelObject {
                name: "right".into(),
                mesh: cube(1.0),
                transform: translate(3.0),
            },
        ],
    };
    mf_io::write_model_path(&model, &path).unwrap();
    path
}

#[test]
fn two_objects_both_visible_and_clicking_one_activates_it() {
    if !mf_render::offscreen::gpu_available() {
        mf_fixtures::skip::gpu_unavailable(
            "two_objects_both_visible_and_clicking_one_activates_it",
        );
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = two_translated_cubes(dir.path());

    let mut h = Harness::builder()
        .with_size((1400.0, 900.0))
        .wgpu()
        .build_eframe(|cc| MeshFixerApp::new(cc, Some(path.clone())).unwrap());
    wait_until(&mut h, "import + analysis", |a| {
        a.session.document().objects().len() == 2 && a.current_analysis().is_some()
    });
    h.run();

    let (first_id, second_id) = {
        let objects = h.state().session.document().objects().to_vec();
        (objects[0].id, objects[1].id)
    };
    assert_eq!(
        h.state().active,
        Some(first_id),
        "the first object imported is active"
    );

    // Select something on the active (first) object, so we can observe it
    // getting cleared when another object becomes active.
    h.state_mut().selection = Some(mf_selection::Selection {
        object: first_id,
        revision: h.state().session.current(),
        kind: mf_selection::SelectionKind::Faces,
        ids: vec![0],
    });
    h.run_steps(2);

    // Both objects must be visible: click where the second (right) object's
    // transformed centre projects, since it's an inactive object in the
    // untransformed-viewport bug this test guards against.
    let world_center_of_second = [3.5, 0.5, 0.5]; // cube centre + translation (3,0,0)
    let pos = h
        .state()
        .screen_point(world_center_of_second)
        .expect("the inactive object's world point must still project to a screen position");
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

    wait_until(&mut h, "click activates the second object", |a| {
        a.active == Some(second_id)
    });
    // The click landed on a face of the second object, so it now has its
    // own (new) selection; the first object's old selection is gone either
    // way, since a selection belongs to exactly one object.
    assert_ne!(
        h.state().selection.as_ref().map(|s| s.object),
        Some(first_id),
        "activating another object clears the first object's selection: {:?}",
        h.state().selection
    );
}
