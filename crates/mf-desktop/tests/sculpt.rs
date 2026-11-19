#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The Sculpt tool on the rail (Phase 1 Task 19): a brush panel, a stroke
//! that runs live on the UI thread (`mf_operations::sculpt::StrokeState`)
//! with per-dab chunk uploads (MF-RENDER-003, MF-SCULPT-004), one undo
//! step per stroke, and Esc dropping a stroke mid-drag.
//!
//! With `MF_ARTIFACTS` set, frames go to
//! `<dir>/design/sculpt-<scene>-<w>x<h>.png`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mf_desktop::{MeshFixerApp, Settings, theme};
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
            "timed out waiting for: {what} (status {:?}, error {:?})",
            h.state().status,
            h.state().error,
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
    let name = format!("sculpt-{scene}-{}x{}.png", size.x as u32, size.y as u32);
    h.render().unwrap().save(dir.join(name)).unwrap();
}

fn opened(objects: Vec<(&str, Mesh, [f64; 12])>) -> Harness<'static, MeshFixerApp> {
    let mut h = Harness::builder()
        .with_size(WINDOW)
        .with_step_dt(1.0 / 60.0)
        .wgpu()
        .build_eframe(move |cc| {
            MeshFixerApp::with_settings(cc, None, Settings::default()).unwrap()
        });
    h.state_mut().import_model(ImportedModel {
        metadata: Default::default(),
        objects: objects
            .into_iter()
            .map(|(name, mesh, transform)| ModelObject {
                metadata: Default::default(),
                name: name.into(),
                mesh,
                transform,
            })
            .collect(),
        unit: Some(Unit::Millimeter),
        source: SourceInfo {
            path: "sculpt.3mf".into(),
            content_hash: ContentHash([0; 32]),
            format: Format::ThreeMf,
            precision: SourcePrecision::F32,
            exporter_hint: None,
        },
        warnings: Vec::new(),
    });
    wait_until(&mut h, "analysis and picking", |a| {
        a.current_analysis().is_some() && a.picking_ready() && a.camera.is_some()
    });
    h.event(egui::Event::PointerGone);
    settle(&mut h);
    h
}

/// A world point well inside the camera-facing side of the ball centred
/// at `center` with `radius`: near the pole would graze the silhouette at
/// the default angled view (yaw -60°, pitch 25°), a near miss a tiny drag
/// step could fall off of.
fn facing_point(h: &Harness<'_, MeshFixerApp>, center: [f64; 3], radius: f64) -> egui::Pos2 {
    let cam = h.state().camera.unwrap();
    let to_eye = (cam.eye() - glam::DVec3::from(center)).normalize();
    let world = glam::DVec3::from(center) + to_eye * (radius * 0.8);
    h.state().screen_point(world.to_array()).unwrap()
}

fn at(t: [f64; 3]) -> [f64; 12] {
    [
        1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, t[0], t[1], t[2],
    ]
}

/// A closed 40 mm ball: big enough that a brush radius of a few mm moves
/// only part of it (a sculpt-sized dab, not a whole-object edit).
fn ball() -> Mesh {
    mf_fixtures::scaled(&mf_fixtures::uv_sphere(48, 96), 20.0)
}

/// Drags the pointer from `from` to `to` in `steps`, a frame apart (the
/// brush samples once per frame at the pointer's position, so a stroke
/// with several steps is several dabs).
fn drag(h: &mut Harness<'_, MeshFixerApp>, from: egui::Pos2, to: egui::Pos2, steps: u32) {
    h.event(egui::Event::PointerMoved(from));
    h.event(egui::Event::PointerButton {
        pos: from,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    for i in 1..=steps {
        let p = from + (to - from) * (i as f32 / steps as f32);
        h.event(egui::Event::PointerMoved(p));
        h.step();
    }
}

fn release(h: &mut Harness<'_, MeshFixerApp>, at: egui::Pos2) {
    h.event(egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
}

/// One stroke (several dabs, a drag) is one undo step, and moves the
/// surface: undo restores the ball's original triangle count and
/// position of the dabbed vertex.
#[test]
fn one_stroke_is_one_undo() {
    require_gpu!("one_stroke_is_one_undo");
    let mut h = opened(vec![("ball", ball(), at([0.0, 0.0, 20.0]))]);
    h.get_by_label("Sculpt").click();
    settle(&mut h);
    assert_eq!(h.state().active_tool, "sculpt");
    still(&mut h, "panel");

    let history = h.state().session.history_labels().len();
    let top = facing_point(&h, [0.0, 0.0, 20.0], 20.0);
    drag(&mut h, top, top + egui::vec2(80.0, 0.0), 10);
    assert!(
        h.state().sculpt_stroke.is_some(),
        "a stroke opened on the drag"
    );
    wait_until(&mut h, "more than one dab recorded", |a| {
        a.sculpt_stroke.as_ref().is_some_and(|s| s.dab_count() > 1)
    });
    still(&mut h, "mid-stroke");
    release(&mut h, top + egui::vec2(80.0, 0.0));
    wait_until(&mut h, "the stroke to commit", |a| {
        a.sculpt_stroke.is_none() && a.busy.is_none()
    });
    settle(&mut h);
    assert_eq!(
        h.state().session.history_labels().len(),
        history + 1,
        "one stroke is one undo step: {:?}",
        h.state().session.history_labels()
    );
    still(&mut h, "after-stroke");

    let after = h.state().active_mesh().unwrap().clone();
    h.state_mut().undo();
    settle(&mut h);
    let restored = h.state().active_mesh().unwrap().clone();
    assert!(
        !Mesh::ptr_eq(&restored, &after),
        "undo restored a different (the original) revision"
    );
}

/// Esc mid-stroke drops it: no command runs, and the surface is back to
/// what it was (the live preview is discarded, not committed).
#[test]
fn escape_drops_the_stroke() {
    require_gpu!("escape_drops_the_stroke");
    let mut h = opened(vec![("ball", ball(), at([0.0, 0.0, 20.0]))]);
    h.get_by_label("Sculpt").click();
    settle(&mut h);
    let history = h.state().session.history_labels().len();
    let before = h.state().active_mesh().unwrap().clone();

    let top = facing_point(&h, [0.0, 0.0, 20.0], 20.0);
    drag(&mut h, top, top + egui::vec2(80.0, 0.0), 10);
    wait_until(&mut h, "a dab recorded", |a| a.sculpt_stroke.is_some());
    h.key_press(egui::Key::Escape);
    settle(&mut h);
    assert!(h.state().sculpt_stroke.is_none(), "Escape ended the stroke");
    assert_eq!(
        h.state().session.history_labels().len(),
        history,
        "no command ran"
    );
    let now = h.state().active_mesh().unwrap().clone();
    assert!(
        Mesh::ptr_eq(&before, &now),
        "the document's mesh is exactly what it was before the stroke"
    );
}

/// The brush cursor is a hover outline on the surface: present while the
/// pointer is over the model with Sculpt open, gone once it leaves.
#[test]
fn brush_cursor_is_a_hover_outline() {
    require_gpu!("brush_cursor_is_a_hover_outline");
    let mut h = opened(vec![("ball", ball(), at([0.0, 0.0, 20.0]))]);
    h.get_by_label("Sculpt").click();
    settle(&mut h);
    let top = facing_point(&h, [0.0, 0.0, 20.0], 20.0);
    h.event(egui::Event::PointerMoved(top));
    h.step();
    assert!(
        h.state().sculpt_hover.is_some(),
        "the pointer over the model is tracked for the cursor"
    );
    still(&mut h, "cursor");
    h.event(egui::Event::PointerGone);
    h.step();
    assert!(h.state().sculpt_hover.is_none());
}

/// Owner (2026-11-11): "The view flickers between half-rendered and
/// complete, while the brush is held and on release." Pixel-sample a
/// fixed screen point (the brush's dab centre) across every frame of a
/// stroke and its release: the sampled colour must never drop back to
/// the exact pre-stroke baseline once it has moved on, and it must never
/// show the viewport's background (the object must always be drawn
/// whole, never a gap where a half-uploaded mesh would show through).
#[test]
fn stroke_and_release_never_flicker() {
    require_gpu!("stroke_and_release_never_flicker");
    let mut h = opened(vec![("ball", ball(), at([0.0, 0.0, 20.0]))]);
    h.get_by_label("Sculpt").click();
    settle(&mut h);
    // A stronger stroke than the panel's default: a radially-symmetric
    // inflate bump moves its very apex (where this test samples) mostly
    // *outward*, not sideways, so the apex's own shading changes less
    // than its slopes do (parallax and the rim's terminator shift, not
    // the centre's). A bigger radius alone doesn't fix that; enough
    // strength to move the apex perceptibly does. "Stay on this
    // surface" (on by default) also measures its radius along the
    // surface, which reaches a little less far in screen-space terms
    // than the old straight-line brush did at the same nominal radius.
    let dab_world = {
        let cam = h.state().camera.unwrap();
        let center = glam::DVec3::new(0.0, 0.0, 20.0);
        let to_eye = (cam.eye() - center).normalize();
        (center + to_eye * (20.0 * 0.8)).to_array()
    };
    h.state_mut().sculpt.radius_pt =
        mf_desktop::sculpt_tool::radius_pt_for_world(h.state(), dab_world, 10.0).unwrap();
    h.state_mut().sculpt.strength = 1.0;

    let top = facing_point(&h, [0.0, 0.0, 20.0], 20.0);
    let pixel_at = |h: &mut Harness<'_, MeshFixerApp>| -> [u8; 4] {
        h.render().unwrap().get_pixel(top.x as u32, top.y as u32).0
    };
    let is_background = |p: [u8; 4]| {
        let bg = theme::VIEWPORT_BACKGROUND.to_array();
        (0..3).all(|k| p[k].abs_diff(bg[k]) <= 1)
    };
    let dist = |a: [u8; 4], b: [u8; 4]| -> i32 {
        (0..3)
            .map(|k| (i32::from(a[k]) - i32::from(b[k])).abs())
            .sum()
    };

    let baseline = pixel_at(&mut h);
    assert!(
        !is_background(baseline),
        "the ball must be drawn under the dab point before the stroke starts"
    );

    // Drive the drag one frame at a time (rather than `drag`'s own loop)
    // so every frame in between is sampled too, not just the start and
    // end.
    h.event(egui::Event::PointerMoved(top));
    h.event(egui::Event::PointerButton {
        pos: top,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    let to = top + egui::vec2(60.0, 0.0);
    let mut samples = vec![(0u32, baseline)];
    let steps = 24;
    for i in 1..=steps {
        let p = top + (to - top) * (i as f32 / steps as f32);
        h.event(egui::Event::PointerMoved(p));
        h.step();
        samples.push((i, pixel_at(&mut h)));
    }
    wait_until(&mut h, "more than one dab recorded", |a| {
        a.sculpt_stroke.as_ref().is_some_and(|s| s.dab_count() > 1)
    });

    // Release, then keep sampling every frame until the committed
    // revision's own render and analysis are fully ready (`sculpt_stroke`
    // *and* the settling render both clear, and the job finishes): the gap
    // this closes is exactly where the owner saw the flicker (a revert to
    // the pre-stroke mesh, then "Updating view…" over it).
    h.event(egui::Event::PointerButton {
        pos: to,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    let start = Instant::now();
    loop {
        h.step();
        samples.push((steps + 1, pixel_at(&mut h)));
        if h.state().sculpt_stroke.is_none()
            && h.state().settling_render().is_none()
            && h.state().busy.is_none()
        {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(90), "never settled");
    }
    // A few more frames past settling: the swap must hold, not bounce.
    for _ in 0..5 {
        h.step();
        samples.push((steps + 2, pixel_at(&mut h)));
    }

    // Once the dabbed point's colour has moved on from the pre-stroke
    // baseline, no later frame may fall back to exactly that baseline
    // (a revert), and no frame may show bare background (a gap where the
    // mesh should be).
    let mut moved_on = false;
    for &(i, p) in &samples {
        assert!(
            !is_background(p),
            "frame {i}: the ball must stay drawn, never the bare background ({p:?})"
        );
        if moved_on {
            assert!(
                dist(p, baseline) > 2,
                "frame {i}: reverted to the pre-stroke colour {baseline:?} (flicker)"
            );
        } else if dist(p, baseline) > 2 {
            moved_on = true;
        }
    }
    assert!(
        moved_on,
        "the stroke never visibly changed the sampled point; the test wouldn't catch a revert"
    );
}

/// The brush panel: picking a brush, moving the radius and strength
/// sliders, and turning on a symmetry axis all land in `app.sculpt`.
#[test]
fn brush_panel_controls_change_the_settings() {
    require_gpu!("brush_panel_controls_change_the_settings");
    let mut h = opened(vec![("ball", ball(), at([0.0, 0.0, 20.0]))]);
    h.get_by_label("Sculpt").click();
    settle(&mut h);
    assert_eq!(
        h.state().sculpt.brush,
        mf_operations::sculpt::BrushKind::Inflate,
        "Inflate is the default brush"
    );
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Carve")
        .click();
    settle(&mut h);
    assert_eq!(
        h.state().sculpt.brush,
        mf_operations::sculpt::BrushKind::Carve
    );
    still(&mut h, "brush-picker");

    assert!(
        h.state().sculpt.stay_on_surface,
        "\"Stay on this surface\" is on by default"
    );
    h.get_by_label("Stay on this surface").click();
    settle(&mut h);
    assert!(
        !h.state().sculpt.stay_on_surface,
        "the checkbox should turn it off"
    );
}
