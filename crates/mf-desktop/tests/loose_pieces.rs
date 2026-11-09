#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Removing loose pieces safely (MF-ISSUE-052): with "Remove loose
//! pieces" open, Properties previews what Apply would remove and the view
//! tints just those pieces; a removal of more than 3% of the triangles (or
//! most pieces and more than 2%) asks first, in plain words.

use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use mf_desktop::MeshFixerApp;
use mf_render::FaceStates;

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
            start.elapsed() < Duration::from_secs(30),
            "timed out waiting for: {what}"
        );
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    h.run_steps(3);
}

/// A 1,512-triangle body, five 12-triangle specks and a 480-triangle part.
fn body_specks_and_part(dir: &std::path::Path) -> std::path::PathBuf {
    let mut parts = vec![mf_fixtures::scaled(&mf_fixtures::uv_sphere(28, 28), 10.0)];
    for i in 0..5 {
        parts.push(mf_fixtures::translated(
            &mf_fixtures::cube(1.0),
            [15.0 + 3.0 * i as f64, 0.0, 0.0],
        ));
    }
    parts.push(mf_fixtures::translated(
        &mf_fixtures::scaled(&mf_fixtures::uv_sphere(16, 16), 4.0),
        [0.0, 20.0, 0.0],
    ));
    let path = dir.join("dress.obj");
    mf_io::write_path(&mf_fixtures::merge(&parts), &path, mf_io::Format::Obj).unwrap();
    path
}

fn state(app: &MeshFixerApp, face: u32) -> u8 {
    app.read_face_state(face).unwrap().unwrap()
}

#[test]
fn remove_loose_pieces_previews_tints_and_asks_before_a_large_removal() {
    require_gpu!("remove_loose_pieces_previews_tints_and_asks_before_a_large_removal");
    let dir = tempfile::tempdir().unwrap();
    let path = body_specks_and_part(dir.path());
    let mut h = Harness::builder()
        .with_size(WINDOW)
        .wgpu()
        .build_eframe(move |cc| MeshFixerApp::new(cc, Some(path.clone())).unwrap());
    wait_until(&mut h, "import + analysis", |a| {
        a.current_analysis().is_some() && a.shows_current_revision()
    });
    h.run();
    let mesh = h.state().active_mesh().unwrap().clone();
    let topo = mesh.topology();
    let part = topo
        .components
        .iter()
        .find(|c| c.len() == 480)
        .unwrap()
        .clone();
    let specks: Vec<u32> = topo
        .components
        .iter()
        .filter(|c| c.len() == 12)
        .flatten()
        .copied()
        .collect();
    // Before the tool is open every loose piece is tinted.
    wait_until(&mut h, "every loose piece tinted", |a| {
        state(a, part[0]) & FaceStates::LOOSE_PIECE != 0
    });

    h.get_by_label("Remove loose pieces").click();
    h.run();
    // The preview, in words: 5 of the 7 pieces, 60 of 2,052 triangles.
    h.get_by_label("Removes 5 of 7 pieces (2.9% of triangles)");
    // Only what it would remove is tinted.
    wait_until(&mut h, "only the candidates tinted", |a| {
        specks
            .iter()
            .all(|&f| state(a, f) & FaceStates::LOOSE_PIECE != 0)
            && part
                .iter()
                .all(|&f| state(a, f) & FaceStates::LOOSE_PIECE == 0)
    });

    // Five of seven pieces and 2.9% of the triangles: Apply asks first.
    let before = h.state().session.current();
    h.get_by_label("Apply").click();
    h.run();
    // The share of the triangles is always given (fix round 1, I3).
    h.get_by_label(
        "This would delete 5 of 7 parts of 'dress' (2.9% of its triangles). These may be \
         part of the design, not debris.",
    );
    // The question is modal: the tree behind it doesn't answer (P4).
    let piece = h
        .get_all_by_role(egui::accesskit::Role::Button)
        .find(|n| {
            n.accesskit_node()
                .label()
                .is_some_and(|l| l.starts_with("Piece "))
        })
        .unwrap()
        .rect()
        .center();
    h.event(egui::Event::PointerMoved(piece));
    h.step();
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos: piece,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
    }
    assert!(h.state().selection.is_none(), "the tree is blocked");
    assert!(h.state().confirmation().is_some(), "still asking");
    // Keep them says how to remove fewer, the right way round (I2).
    let keep = h.get_by_label("Keep them").rect().center();
    h.event(egui::Event::PointerMoved(keep));
    for _ in 0..60 {
        h.step();
    }
    h.get_by_label("Esc. To remove fewer, lower Max relative size or Max triangles.");
    h.get_by_label("Keep them").click();
    h.run();
    assert_eq!(h.state().session.current(), before, "nothing removed");
    assert!(h.state().busy.is_none());

    h.get_by_label("Apply").click();
    h.run();
    h.get_by_label("Remove them").click();
    wait_until(&mut h, "the removal", |a| {
        a.session.current() != before && a.busy.is_none()
    });
    let after = h.state().active_mesh().unwrap().topology().components.len();
    assert_eq!(after, 2, "the body and the part are kept");
}
