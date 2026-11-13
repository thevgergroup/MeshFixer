#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Navigation without a mouse wheel (M4 Task 2a): an on-screen navigation
//! bar (zoom in/out, frame, reset, Orbit/Pan/Zoom drag modes), zoom and
//! reset keys, and modifier drags, so trackpads and one-button devices can
//! orbit, pan and zoom.

use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mf_desktop::MeshFixerApp;
use mf_geometry::FaceId;
use mf_render::OrbitCamera;

const WINDOW: (f32, f32) = (1440.0, 900.0);

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

fn open() -> (tempfile::TempDir, Harness<'static, MeshFixerApp>) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("holed.obj");
    let n = 6;
    let mut e = mf_fixtures::grid(n).edit();
    e.remove_face(FaceId(2 * (n + 1))).unwrap();
    let (m, _) = e.finish().unwrap();
    mf_io::write_path(
        &m,
        &path,
        mf_io::Format::Obj,
        mf_io::threemf::Unit::Millimeter,
    )
    .unwrap();
    let mut h = Harness::builder()
        .with_size(WINDOW)
        .with_max_steps(16)
        .wgpu()
        .build_eframe(move |cc| MeshFixerApp::new(cc, Some(path.clone())).unwrap());
    wait_until(&mut h, "import + analysis", |a| {
        a.current_analysis().is_some()
    });
    // Settle so the accessibility check runs (every new control is labelled).
    h.run();
    (dir, h)
}

macro_rules! require_gpu {
    ($name:literal) => {
        if !mf_render::offscreen::gpu_available() {
            mf_fixtures::skip::gpu_unavailable($name);
            return;
        }
    };
}

fn cam(h: &Harness<'_, MeshFixerApp>) -> OrbitCamera {
    h.state().camera.unwrap()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(b.abs())
}

/// A primary-button drag in the viewport from its centre by `delta`, with
/// `modifiers` held throughout.
fn drag(h: &mut Harness<'_, MeshFixerApp>, delta: egui::Vec2, modifiers: egui::Modifiers) {
    let view = h.state().viewport.last_rect.unwrap();
    let start = view.center() + egui::vec2(-view.width() * 0.15, 0.0);
    h.event(egui::Event::ModifiersChanged(modifiers));
    h.event(egui::Event::PointerMoved(start));
    h.event(egui::Event::PointerButton {
        pos: start,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers,
    });
    for k in 1..=10 {
        h.event(egui::Event::PointerMoved(start + delta * (k as f32 / 10.0)));
    }
    h.event(egui::Event::PointerButton {
        pos: start + delta,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers,
    });
    h.event(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    h.run();
}

#[test]
fn zoom_buttons_change_distance_and_keep_target() {
    require_gpu!("zoom_buttons_change_distance_and_keep_target");
    let (_dir, mut h) = open();
    let start = cam(&h);
    let view = h.state().viewport.last_rect.unwrap();

    // The bar sits inside the 3D view, at its bottom-right.
    let zoom_in = h.get_by_label("Zoom in").rect();
    assert!(view.contains_rect(zoom_in), "{zoom_in:?} inside {view:?}");
    assert!(zoom_in.center().x > view.center().x && zoom_in.center().y > view.center().y);
    for label in [
        "Zoom out",
        "Frame selection",
        "Reset view",
        "Orbit",
        "Pan",
        "Zoom",
    ] {
        assert!(view.contains_rect(h.get_by_label(label).rect()), "{label}");
    }

    h.get_by_label("Zoom in").click();
    h.run();
    let zoomed = cam(&h);
    assert!(
        close(zoomed.distance, start.distance / 1.25),
        "{}",
        zoomed.distance
    );
    assert_eq!(zoomed.target, start.target, "zooms toward the view centre");
    assert_eq!((zoomed.yaw, zoomed.pitch), (start.yaw, start.pitch));

    h.get_by_label("Zoom out").click();
    h.run();
    assert!(close(cam(&h).distance, start.distance));
    assert_eq!(cam(&h).target, start.target);

    // Holding the button repeats.
    let pos = h.get_by_label("Zoom in").rect().center();
    h.event(egui::Event::PointerMoved(pos));
    h.event(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    let held = Instant::now();
    while held.elapsed() < Duration::from_millis(900) {
        h.step();
        std::thread::sleep(Duration::from_millis(15));
    }
    h.event(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.run();
    assert!(
        cam(&h).distance < start.distance / 1.25 / 1.25,
        "holding zooms more than once: {} vs {}",
        cam(&h).distance,
        start.distance
    );
    assert_eq!(cam(&h).target, start.target);
}

#[test]
fn plus_minus_keys_zoom_and_cmd0_resets() {
    require_gpu!("plus_minus_keys_zoom_and_cmd0_resets");
    let (_dir, mut h) = open();
    let start = cam(&h);

    h.key_press(egui::Key::Plus);
    h.run();
    assert!(close(cam(&h).distance, start.distance / 1.25));
    h.key_press(egui::Key::Equals);
    h.run();
    assert!(close(cam(&h).distance, start.distance / 1.25 / 1.25));
    h.key_press(egui::Key::Minus);
    h.run();
    assert!(close(cam(&h).distance, start.distance / 1.25));
    assert_eq!(cam(&h).target, start.target);

    // Move the view, then Cmd/Ctrl+0 resets it like Home.
    drag(&mut h, egui::vec2(120.0, 40.0), egui::Modifiers::NONE);
    assert_ne!(cam(&h).yaw, start.yaw);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Num0);
    h.run();
    let reset = cam(&h);
    assert_eq!((reset.yaw, reset.pitch), (start.yaw, start.pitch));
    assert!(close(reset.distance, start.distance));
    assert_eq!(reset.target, start.target);
}

#[test]
fn zoom_mode_drag_zooms_and_pan_mode_drag_pans() {
    require_gpu!("zoom_mode_drag_zooms_and_pan_mode_drag_pans");
    let (_dir, mut h) = open();
    let start = cam(&h);

    // Orbit is the default mode.
    drag(&mut h, egui::vec2(80.0, 0.0), egui::Modifiers::NONE);
    let orbited = cam(&h);
    assert_ne!(orbited.yaw, start.yaw);
    assert_eq!(orbited.distance, start.distance);

    // Zoom mode: a vertical drag zooms (up = in) toward the drag start.
    h.get_by_label("Zoom").click();
    h.run();
    drag(&mut h, egui::vec2(0.0, -80.0), egui::Modifiers::NONE);
    let zoomed = cam(&h);
    assert!(zoomed.distance < orbited.distance, "drag up zooms in");
    assert_eq!((zoomed.yaw, zoomed.pitch), (orbited.yaw, orbited.pitch));
    assert_ne!(
        zoomed.target, orbited.target,
        "toward the drag start, off centre"
    );

    // Pan mode: a drag pans.
    h.get_by_label("Pan").click();
    h.run();
    drag(&mut h, egui::vec2(60.0, 30.0), egui::Modifiers::NONE);
    let panned = cam(&h);
    assert_ne!(panned.target, zoomed.target);
    assert_eq!((panned.yaw, panned.pitch), (zoomed.yaw, zoomed.pitch));
    assert_eq!(panned.distance, zoomed.distance);

    // Back to Orbit.
    h.get_by_label("Orbit").click();
    h.run();
    drag(&mut h, egui::vec2(80.0, 0.0), egui::Modifiers::NONE);
    assert_ne!(cam(&h).yaw, panned.yaw);
}

#[test]
fn alt_drag_zooms_and_shift_drag_pans() {
    require_gpu!("alt_drag_zooms_and_shift_drag_pans");
    let (_dir, mut h) = open();
    let start = cam(&h);

    drag(&mut h, egui::vec2(0.0, 80.0), egui::Modifiers::ALT);
    let zoomed = cam(&h);
    assert!(
        zoomed.distance > start.distance,
        "Option/Alt + drag down zooms out"
    );
    assert_eq!((zoomed.yaw, zoomed.pitch), (start.yaw, start.pitch));

    drag(&mut h, egui::vec2(60.0, 30.0), egui::Modifiers::SHIFT);
    let panned = cam(&h);
    assert_ne!(panned.target, zoomed.target);
    assert_eq!((panned.yaw, panned.pitch), (zoomed.yaw, zoomed.pitch));
    assert_eq!(panned.distance, zoomed.distance);
}

#[test]
fn navigation_keys_ignored_while_typing_in_a_field() {
    require_gpu!("navigation_keys_ignored_while_typing_in_a_field");
    let (_dir, mut h) = open();
    let start = cam(&h);

    // A text field has focus: open the "Fill all holes" tool and focus its
    // number field.
    h.get_by_label("Fill all holes").click();
    h.run();
    h.get_by_role(egui::accesskit::Role::SpinButton).focus();
    h.run();
    for key in [egui::Key::Plus, egui::Key::Minus, egui::Key::Equals] {
        h.key_press(key);
        h.run();
    }
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Num0);
    h.run();
    assert_eq!(cam(&h), start, "keys went to the field, not the view");

    // A popup (an open menu) has focus.
    h.key_press(egui::Key::Escape);
    h.run();
    h.get_by_label("View").click();
    h.run();
    h.key_press(egui::Key::Plus);
    h.run();
    assert_eq!(cam(&h), start, "keys went to the menu, not the view");
}
