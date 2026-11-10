#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The look-and-feel frames (Phase 1 design stream): real app frames of
//! each state the owner judges the design from, at 1440×900 and 1280×720.
//! An empty document, no tool active (a watertight part and a scan with
//! defects), Transform in each mode, Repair with holes, and Inspect.
//!
//! Each test also checks the state it pictures is the one on screen. With
//! `MF_ARTIFACTS=<dir>` set, each frame is written to
//! `<dir>/design/<stage>-<scene>-<w>x<h>.png`, `<stage>` being
//! `MF_DESIGN_STAGE` (default `after`).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mf_desktop::{MeshFixerApp, Settings};
use mf_geometry::{ContentHash, Mesh};
use mf_io::threemf::{IDENTITY, ModelObject, Unit};
use mf_io::{Format, ImportedModel, SourceInfo, SourcePrecision};

const SIZES: [(f32, f32); 2] = [(1440.0, 900.0), (1280.0, 720.0)];

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
            "timed out waiting for: {what} (status {:?}, error {:?})",
            h.state().status,
            h.state().error,
        );
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Steps until no background work is in flight and nothing asks for a
/// frame (wall time), then a few more.
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

/// Saves the frame as `<MF_ARTIFACTS>/design/<stage>-<scene>-<w>x<h>.png`.
fn still(h: &mut Harness<'_, MeshFixerApp>, scene: &str, size: (f32, f32)) {
    let Some(dir) = std::env::var_os("MF_ARTIFACTS") else {
        return;
    };
    let stage = std::env::var("MF_DESIGN_STAGE").unwrap_or_else(|_| "after".into());
    let dir = PathBuf::from(dir).join("design");
    std::fs::create_dir_all(&dir).unwrap();
    let name = format!("{stage}-{scene}-{}x{}.png", size.0 as u32, size.1 as u32);
    h.render().unwrap().save(dir.join(name)).unwrap();
}

fn harness(size: (f32, f32)) -> Harness<'static, MeshFixerApp> {
    Harness::builder()
        .with_size(size)
        .with_step_dt(1.0 / 60.0)
        .wgpu()
        .build_eframe(move |cc| MeshFixerApp::with_settings(cc, None, Settings::default()).unwrap())
}

fn model(objects: Vec<(&str, Mesh)>) -> ImportedModel {
    ImportedModel {
        objects: objects
            .into_iter()
            .map(|(name, mesh)| ModelObject {
                name: name.into(),
                mesh,
                transform: IDENTITY,
            })
            .collect(),
        unit: Unit::Millimeter,
        source: SourceInfo {
            path: "design.3mf".into(),
            content_hash: ContentHash([0; 32]),
            format: Format::ThreeMf,
            precision: SourcePrecision::F32,
        },
        warnings: Vec::new(),
    }
}

/// A 40 × 16 × 10 mm block resting on the plate: a watertight part.
fn part() -> Mesh {
    let (positions, triangles) = mf_fixtures::cube(10.0).to_arrays();
    let shaped = positions
        .iter()
        .map(|p| [p[0] * 4.0, p[1] * 1.6, p[2]])
        .collect();
    Mesh::new(shaped, triangles).unwrap()
}

/// A scan: a damaged sphere 40 mm across (four holes) with three loose
/// specks floating off it.
fn scan() -> Mesh {
    let sphere = mf_fixtures::scaled(
        &mf_fixtures::damaged_sphere(&mf_fixtures::DamageSpec {
            rings: 32,
            segments: 64,
            holes: 4,
            islands: 0,
            unwelded: 0,
            seed: 3,
        }),
        20.0,
    );
    let mut parts = vec![sphere];
    for d in [[23.0, 0.0, 4.0], [-6.0, 22.0, -8.0], [4.0, -9.0, 21.0]] {
        parts.push(mf_fixtures::translated(&mf_fixtures::cube(2.0), d));
    }
    mf_fixtures::merge(&parts)
}

fn opened(size: (f32, f32), objects: Vec<(&str, Mesh)>) -> Harness<'static, MeshFixerApp> {
    let mut h = harness(size);
    h.state_mut().import_model(model(objects));
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

#[test]
fn design_frame_empty_document() {
    require_gpu!("design_frame_empty_document");
    for size in SIZES {
        let mut h = harness(size);
        settle(&mut h);
        assert!(h.state().active.is_none());
        still(&mut h, "empty", size);
    }
}

#[test]
fn design_frame_no_tool() {
    require_gpu!("design_frame_no_tool");
    for size in SIZES {
        let mut h = opened(size, vec![("bracket", part())]);
        assert!(h.state().interactive.is_none());
        still(&mut h, "no-tool-part", size);
        let mut h = opened(size, vec![("scan", scan())]);
        still(&mut h, "no-tool-scan", size);
    }
}

#[test]
fn design_frame_transform_modes() {
    require_gpu!("design_frame_transform_modes");
    for size in SIZES {
        let mut h = opened(size, vec![("bracket", part())]);
        h.key_press(egui::Key::T);
        wait_until(&mut h, "the gizmo", |a| a.transform_gizmo().is_some());
        settle(&mut h);
        for (key, mode) in [
            (egui::Key::W, "move"),
            (egui::Key::E, "rotate"),
            (egui::Key::R, "scale"),
        ] {
            h.key_press(key);
            settle(&mut h);
            still(&mut h, &format!("transform-{mode}"), size);
        }
    }
}

#[test]
fn design_frame_repair_with_holes() {
    require_gpu!("design_frame_repair_with_holes");
    for size in SIZES {
        let mut h = opened(size, vec![("scan", scan())]);
        h.get_by_label("Repair").click();
        settle(&mut h);
        assert_eq!(h.state().active_tool, "repair");
        still(&mut h, "repair", size);
        h.get_by_label("Fill all holes").click();
        settle(&mut h);
        h.get_by_label("Apply");
        still(&mut h, "repair-fill", size);
        // A hole picked in the Browser: Repair shows it, with Fill hole.
        h.key_press(egui::Key::Escape);
        settle(&mut h);
        h.key_press(egui::Key::N);
        settle(&mut h);
        h.get_by_label("Apply");
        still(&mut h, "repair-hole", size);
    }
}

#[test]
fn design_frame_inspect_and_export() {
    require_gpu!("design_frame_inspect_and_export");
    for size in SIZES {
        let mut h = opened(size, vec![("scan", scan())]);
        h.get_by_label("Inspect").click();
        settle(&mut h);
        // Owner report: no label wraps mid-word. Each row is one line,
        // label left and value right, the technical term in its tooltip.
        for label in [
            "Triangles: ",
            "Pinched points: ",
            "Near-duplicate points: ",
            "Untraced open edges: ",
        ] {
            let row = h.get_by_label_contains(label).rect();
            assert!(row.height() <= 24.0, "{label} wraps at {size:?}: {row:?}");
        }
        still(&mut h, "inspect", size);
        h.get_by_label("Export").click();
        settle(&mut h);
        still(&mut h, "export", size);
    }
}

#[test]
fn design_frame_overlays_and_history() {
    require_gpu!("design_frame_overlays_and_history");
    let size = SIZES[0];
    let mut h = opened(size, vec![("scan", scan())]);
    h.get_by_label("Overlays").click();
    settle(&mut h);
    h.get_by_label("Recent repairs");
    h.get_by_label("History").click();
    settle(&mut h);
    still(&mut h, "overlays-history", size);
}

/// A compact window (1024×640): icons-only rail, the Browser a drawer.
#[test]
fn design_frame_compact_window() {
    require_gpu!("design_frame_compact_window");
    let size = (1024.0, 640.0);
    let mut h = opened(size, vec![("scan", scan())]);
    still(&mut h, "no-tool-scan", size);
    h.key_press(egui::Key::T);
    settle(&mut h);
    still(&mut h, "transform-move", size);
}
