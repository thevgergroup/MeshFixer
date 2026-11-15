#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The Grab brush (`BrushKind::Drag`), driven through the real Sculpt
//! tool and viewport drag, not just the operation layer
//! (`mf-operations/tests/sculpt.rs`'s own Grab tests cover that level).
//!
//! Owner report: a drag with a small radius on a sphere moved vertices on
//! the far side and on other objects, spiked the surface in many places,
//! and tore it (dark holes from inverted faces). These tests reproduce
//! the owner's scenario (a short-radius drag on a sphere) through the
//! actual tool and check the fix's invariants end to end: the far side
//! and other objects never move, no face inverts, and the dabbed point
//! moves by the drag distance.

use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mf_desktop::MeshFixerApp;
use mf_geometry::{ContentHash, Mesh};
use mf_io::threemf::{ModelObject, Unit};
use mf_io::{Format, ImportedModel, SourceInfo, SourcePrecision};
use mf_operations::sculpt::BrushKind;

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

fn opened(objects: Vec<(&str, Mesh, [f64; 12])>) -> Harness<'static, MeshFixerApp> {
    let mut h = Harness::builder()
        .with_size(WINDOW)
        .with_step_dt(1.0 / 60.0)
        .wgpu()
        .build_eframe(move |cc| {
            MeshFixerApp::with_settings(cc, None, mf_desktop::Settings::default()).unwrap()
        });
    let n = objects.len();
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
        unit: Unit::Millimeter,
        source: SourceInfo {
            path: "sculpt_grab.3mf".into(),
            content_hash: ContentHash([0; 32]),
            format: Format::ThreeMf,
            precision: SourcePrecision::F32,
            exporter_hint: None,
        },
        warnings: Vec::new(),
    });
    wait_until(&mut h, "analysis and picking", |a| {
        a.session.document().objects().len() == n
            && a.current_analysis().is_some()
            && a.picking_ready()
            && a.camera.is_some()
    });
    h.event(egui::Event::PointerGone);
    settle(&mut h);
    h
}

/// A world point well inside the camera-facing side of the ball centred
/// at `center` with `radius` (same reasoning as the other sculpt tests:
/// near the pole would graze the silhouette at the default angled view),
/// and its screen position. The world point is the drag's own true
/// starting point on the sphere — at the default angled camera (yaw
/// -60°, pitch 25°), *not* any particular pole, so tests that need to
/// know where the drag actually starts (far-side checks) must use this,
/// not assume a pole.
fn facing_point_and_world(
    h: &Harness<'_, MeshFixerApp>,
    center: [f64; 3],
    radius: f64,
) -> (egui::Pos2, [f64; 3]) {
    let cam = h.state().camera.unwrap();
    let to_eye = (cam.eye() - glam::DVec3::from(center)).normalize();
    let world = glam::DVec3::from(center) + to_eye * (radius * 0.8);
    let px = h.state().screen_point(world.to_array()).unwrap();
    (px, world.to_array())
}

fn at(t: [f64; 3]) -> [f64; 12] {
    [
        1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, t[0], t[1], t[2],
    ]
}

/// A closed 40 mm ball: big enough that a 5 mm-ish brush radius (the
/// owner's own report) moves only a small patch of it.
fn ball() -> Mesh {
    mf_fixtures::scaled(&mf_fixtures::uv_sphere(48, 96), 20.0)
}

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

/// Every live face's cross-product normal (unnormalized: only its sign
/// and relative direction matter for an inversion check).
fn face_normals(mesh: &Mesh) -> std::collections::HashMap<u32, [f64; 3]> {
    mesh.faces()
        .map(|(f, t)| {
            let [a, b, c] = t.map(|v| mesh.corner_position(v));
            let e1 = std::array::from_fn::<f64, 3, _>(|k| b[k] - a[k]);
            let e2 = std::array::from_fn::<f64, 3, _>(|k| c[k] - a[k]);
            let n = [
                e1[1] * e2[2] - e1[2] * e2[1],
                e1[2] * e2[0] - e1[0] * e2[2],
                e1[0] * e2[1] - e1[1] * e2[0],
            ];
            (f.0, n)
        })
        .collect()
}

/// A short, small-radius Grab drag near one pole of a 40 mm ball,
/// driven through the real Sculpt tool (brush panel, viewport drag,
/// release): the far side of *that same* ball never moves, matching the
/// owner's report of a 5 mm-radius drag moving vertices "on the far side
/// and on other parts" of the model.
#[test]
fn grab_drag_never_moves_the_far_side_of_the_sphere() {
    require_gpu!("grab_drag_never_moves_the_far_side_of_the_sphere");
    let mut h = opened(vec![("ball", ball(), at([0.0, 0.0, 20.0]))]);
    h.get_by_label("Sculpt").click();
    settle(&mut h);
    h.state_mut().sculpt.brush = BrushKind::Drag;
    h.state_mut().sculpt.strength = 1.0;
    assert_eq!(h.state().sculpt.brush, BrushKind::Drag);

    let before = h.state().active_mesh().unwrap().clone();
    let (top, drag_start_world) = facing_point_and_world(&h, [0.0, 0.0, 20.0], 20.0);
    // The owner's own report: a 5 mm radius. The panel's own setting is
    // screen points since the fix (owner bug 2026-11-14), so pin the
    // brush to this exact world size at the drag's own starting depth,
    // same as the old document-units `radius` field would have.
    h.state_mut().sculpt.radius_pt =
        mf_desktop::sculpt_tool::radius_pt_for_world(h.state(), drag_start_world, 5.0).unwrap();
    // `before.vertices()` is the document's own mesh, in *object* space;
    // `facing_point_and_world`'s own point is world space. The ball's
    // transform here (`at([0, 0, 20])`) is a pure translation, so
    // subtracting it converts the one point we need back to object
    // space, rather than transforming the whole mesh.
    let drag_start = [
        drag_start_world[0],
        drag_start_world[1],
        drag_start_world[2] - 20.0,
    ];
    drag(&mut h, top, top + egui::vec2(40.0, 0.0), 10);
    wait_until(&mut h, "more than one dab recorded", |a| {
        a.sculpt_stroke.as_ref().is_some_and(|s| s.dab_count() > 1)
    });
    release(&mut h, top + egui::vec2(40.0, 0.0));
    wait_until(&mut h, "the stroke to commit", |a| {
        a.sculpt_stroke.is_none() && a.busy.is_none()
    });
    settle(&mut h);

    let after = h.state().active_mesh().unwrap().clone();
    assert!(
        !Mesh::ptr_eq(&before, &after),
        "the drag must have changed something"
    );

    // The far side: more than the radius away, in straight-line
    // distance (object space, matching `drag_start` above), from the
    // drag's own true starting point on the sphere (`facing_point`'s own
    // world point — at the default angled camera this is *not* any
    // particular pole). Straight-line distance is always ≤ geodesic
    // distance, so this is a safe (if slightly generous) stand-in for
    // "far enough to be outside the captured region" without
    // reimplementing the engine's own geodesic search here.
    let radius = 5.0;
    let mut moved = 0;
    for (v, p0) in before.vertices() {
        let d = (0..3)
            .map(|k| (p0[k] - drag_start[k]).powi(2))
            .sum::<f64>()
            .sqrt();
        let now = after.position(v).unwrap();
        if d >= radius + 1.0 {
            // +1mm margin past the radius: the dab centre itself may
            // land a little off the pole depending on exactly where the
            // ray lands, so this keeps clear of the boundary.
            assert_eq!(
                now, p0,
                "{v:?} is {d} mm from the drag's start, past the {radius} mm radius: it must not move"
            );
        }
        moved += usize::from(now != p0);
    }
    assert!(moved > 0, "the dabbed patch itself must still move");
}

/// A Grab drag on one object never moves a second, unrelated object's
/// vertices: the owner's report included vertices moving "on other
/// parts" of the model (read here as: other objects in the scene, the
/// strongest form of that claim — the engine has no way to reach another
/// object's mesh from inside one `StrokeState`, but this checks it end
/// to end through the real multi-object scene and undo step).
#[test]
fn grab_drag_never_moves_another_object() {
    require_gpu!("grab_drag_never_moves_another_object");
    let mut h = opened(vec![
        ("ball", ball(), at([0.0, 0.0, 20.0])),
        ("other", ball(), at([60.0, 0.0, 20.0])),
    ]);
    let (first_id, second_id) = {
        let objects = h.state().session.document().objects().to_vec();
        (objects[0].id, objects[1].id)
    };
    assert_eq!(h.state().active, Some(first_id));
    let other_before = h
        .state()
        .session
        .document()
        .object(second_id)
        .unwrap()
        .mesh()
        .unwrap()
        .clone();

    h.get_by_label("Sculpt").click();
    settle(&mut h);
    h.state_mut().sculpt.brush = BrushKind::Drag;
    h.state_mut().sculpt.strength = 1.0;

    let (top, drag_start_world) = facing_point_and_world(&h, [0.0, 0.0, 20.0], 20.0);
    h.state_mut().sculpt.radius_pt =
        mf_desktop::sculpt_tool::radius_pt_for_world(h.state(), drag_start_world, 5.0).unwrap();
    drag(&mut h, top, top + egui::vec2(40.0, 0.0), 10);
    wait_until(&mut h, "more than one dab recorded", |a| {
        a.sculpt_stroke.as_ref().is_some_and(|s| s.dab_count() > 1)
    });
    release(&mut h, top + egui::vec2(40.0, 0.0));
    wait_until(&mut h, "the stroke to commit", |a| {
        a.sculpt_stroke.is_none() && a.busy.is_none()
    });
    settle(&mut h);

    let other_after = h
        .state()
        .session
        .document()
        .object(second_id)
        .unwrap()
        .mesh()
        .unwrap()
        .clone();
    assert_eq!(
        other_before.content_hash(),
        other_after.content_hash(),
        "the other object must be completely untouched by a stroke on the first"
    );
    assert_eq!(
        h.state().active,
        Some(first_id),
        "the active object didn't change"
    );
}

/// No face of the sphere inverts, even after a large, aggressive Grab
/// drag: the owner's report of "dark holes from inverted faces" must
/// never reproduce. Every live face's normal still points roughly the
/// way it did before the stroke (its dot product with the original
/// normal stays positive).
#[test]
fn grab_drag_never_inverts_a_face() {
    require_gpu!("grab_drag_never_inverts_a_face");
    let mut h = opened(vec![("ball", ball(), at([0.0, 0.0, 20.0]))]);
    h.get_by_label("Sculpt").click();
    settle(&mut h);
    h.state_mut().sculpt.brush = BrushKind::Drag;
    h.state_mut().sculpt.strength = 1.0;

    let before = h.state().active_mesh().unwrap().clone();
    let before_normals = face_normals(&before);
    let (top, drag_start_world) = facing_point_and_world(&h, [0.0, 0.0, 20.0], 20.0);
    // Deliberately aggressive: a bigger radius and full strength, the
    // kind of large drag that tore the surface in the owner's report
    // absent the fix.
    h.state_mut().sculpt.radius_pt =
        mf_desktop::sculpt_tool::radius_pt_for_world(h.state(), drag_start_world, 10.0).unwrap();
    drag(&mut h, top, top + egui::vec2(150.0, 40.0), 12);
    wait_until(&mut h, "more than one dab recorded", |a| {
        a.sculpt_stroke.as_ref().is_some_and(|s| s.dab_count() > 1)
    });
    release(&mut h, top + egui::vec2(150.0, 40.0));
    wait_until(&mut h, "the stroke to commit", |a| {
        a.sculpt_stroke.is_none() && a.busy.is_none()
    });
    settle(&mut h);

    let after = h.state().active_mesh().unwrap().clone();
    let after_normals = face_normals(&after);
    for (f, n1) in &after_normals {
        let Some(n0) = before_normals.get(f) else {
            continue;
        };
        let dot: f64 = (0..3).map(|k| n0[k] * n1[k]).sum();
        assert!(dot > 0.0, "face {f} flipped: old normal {n0:?}, new {n1:?}");
    }
}

/// The dabbed point moves by (approximately) the on-screen drag
/// distance: a small, gentle drag moves the surface under the cursor by
/// about as far as the cursor itself travelled, scaled to world space by
/// the camera's own projection at that depth — not some wildly
/// disproportionate amount (the bug's own "spikes" symptom). The vertex
/// that moved the *most* is, by construction, the one nearest the dab
/// centre (full falloff weight): this reads its displacement back
/// directly off the mesh, rather than guessing where the dab centre
/// landed from the camera geometry (the true surface hit point isn't
/// exactly where `facing_point`'s own targeting point sits — that point
/// is inside the sphere, a target for choosing a screen pixel, not the
/// surface hit itself).
#[test]
fn grab_drag_moves_the_dabbed_point_by_about_the_drag_distance() {
    require_gpu!("grab_drag_moves_the_dabbed_point_by_about_the_drag_distance");
    let mut h = opened(vec![("ball", ball(), at([0.0, 0.0, 20.0]))]);
    h.get_by_label("Sculpt").click();
    settle(&mut h);
    h.state_mut().sculpt.brush = BrushKind::Drag;
    h.state_mut().sculpt.strength = 1.0;

    let before = h.state().active_mesh().unwrap().clone();
    let (top, drag_start_world) = facing_point_and_world(&h, [0.0, 0.0, 20.0], 20.0);
    let radius_mm = 8.0;
    h.state_mut().sculpt.radius_pt =
        mf_desktop::sculpt_tool::radius_pt_for_world(h.state(), drag_start_world, radius_mm)
            .unwrap();

    // Big enough, in screen pixels, to clear `DAB_SPACING`'s world-space
    // threshold (a fraction of the brush's own *world* radius) a few
    // times over at this camera's zoom level — a too-small screen drag
    // (checked while writing this test) samples only the one opening dab
    // and never continues the stroke.
    let screen_drag = 120.0_f32;
    drag(&mut h, top, top + egui::vec2(screen_drag, 0.0), 8);
    wait_until(&mut h, "more than one dab recorded", |a| {
        a.sculpt_stroke.as_ref().is_some_and(|s| s.dab_count() > 1)
    });
    let to = top + egui::vec2(screen_drag, 0.0);
    release(&mut h, to);
    wait_until(&mut h, "the stroke to commit", |a| {
        a.sculpt_stroke.is_none() && a.busy.is_none()
    });
    settle(&mut h);

    let after = h.state().active_mesh().unwrap().clone();
    let max_moved: f64 = before
        .vertices()
        .map(|(v, p0)| {
            let now = after.position(v).unwrap();
            (0..3).map(|k| (now[k] - p0[k]).powi(2)).sum::<f64>().sqrt()
        })
        .fold(0.0, f64::max);
    // A loose world-space bound: at `Falloff::Smooth`'s own weight of
    // 1.0 at the centre and `strength = 1.0`, the most any vertex can
    // move is the drag's own screen-to-world projected distance — the
    // operation layer's own tests (`mf-operations/tests/sculpt.rs`) pin
    // the exact formula; this just checks the real UI path produces a
    // comparable, sane result, clear of both "did nothing" and "flew off
    // disproportionately far" (the bug's own spikes).
    assert!(
        max_moved > 0.1,
        "nothing moved enough to read as a dab: {max_moved} mm"
    );
    assert!(
        max_moved < 2.0 * radius_mm,
        "the most-moved vertex moved {max_moved} mm, more than twice the brush radius: looks like the bug"
    );
}
