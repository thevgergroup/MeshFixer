#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Panel migration B (consolidation plan C17, units-audit.md §6.4-6.5,
//! ADR-0006): Split, Make solid, Hollow and Inspect show and edit lengths
//! through `widgets::length_field`, with scale-relative defaults.
//!
//! - Dragging the split point's label on the metres bunny moves it by
//!   millimetres (a `nice_step` at this zoom), not 0.5 m per point.
//! - A 2 mm wall on a metres document shows "2.00 mm", not "0".
//! - A sub-millimetre Make solid cell on a metres document keeps its value.
//! - Inspect shows lengths, areas and volumes with a unit.
//! - The defaults are sane on a 10 mm part, the 0.156 m bunny and a 3 m
//!   part: the cell is diagonal/256, the wall the spec's 2 mm, the split
//!   bound half the diagonal and the split box the machine profile's print
//!   volume, all physically the same whatever the document's unit.

use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use glam::DVec3;
use mf_desktop::{MeshFixerApp, Settings};
use mf_geometry::{ContentHash, Mesh};
use mf_io::threemf::{ModelObject, Unit};
use mf_io::{Format, ImportedModel, SourceInfo, SourcePrecision};
use mf_units::Length;

const WINDOW: (f32, f32) = (1440.0, 1000.0);

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

fn at(t: [f64; 3]) -> [f64; 12] {
    [
        1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, t[0], t[1], t[2],
    ]
}

/// A settings folder of its own (no real Machine profiles leak in): the
/// uncalibrated profile's print volume is the one the box defaults to.
fn isolated_settings_dir() -> &'static std::path::Path {
    static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    DIR.get_or_init(|| tempfile::tempdir().expect("a temp dir for test settings"))
        .path()
}

fn opened_at(name: &str, mesh: Mesh, unit: Unit) -> Harness<'static, MeshFixerApp> {
    let settings = Settings {
        config_dir: Some(isolated_settings_dir().to_owned()),
        ..Settings::default()
    };
    let mut h = Harness::builder()
        .with_size(WINDOW)
        .with_step_dt(1.0 / 60.0)
        .wgpu()
        .build_eframe(move |cc| MeshFixerApp::with_settings(cc, None, settings).unwrap());
    h.state_mut().import_model(ImportedModel {
        metadata: Default::default(),
        objects: vec![ModelObject {
            metadata: Default::default(),
            name: name.into(),
            mesh,
            transform: at([0.0; 3]),
        }],
        unit: Some(unit),
        source: SourceInfo {
            path: "panel-units.3mf".into(),
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

/// The raw Stanford bunny scan, metre-scale (0.156 m long), opened as a
/// metres document. `None` (a recorded skip) without the fixture.
fn bunny_in_metres(test: &str) -> Option<Harness<'static, MeshFixerApp>> {
    let path = mf_fixtures::real::path("stanford-bunny.obj", test)?;
    let model = mf_io::read_model(&path).unwrap();
    let bunny = model.objects.into_iter().next().unwrap().mesh;
    Some(opened_at("bunny", bunny, Unit::Meter))
}

/// A cube `side_mm` millimetres on a side, in a `unit` document.
fn cube_in(side_mm: f64, unit: Unit) -> Harness<'static, MeshFixerApp> {
    let side = Length::new(side_mm, Unit::Millimeter).in_doc(unit);
    opened_at("part", mf_fixtures::cube(side), unit)
}

/// The text a length field shows (its accessible value).
fn shown(h: &Harness<'_, MeshFixerApp>, label: &str) -> String {
    h.get_by_label(label).value().unwrap()
}

/// Drags the label in front of the field `label` (its scrub handle) by
/// `dx` points, in small moves.
fn scrub(h: &mut Harness<'_, MeshFixerApp>, label: &str, dx: f32) {
    let field = h.get_by_label(label).rect();
    let from = egui::pos2(field.left() - 12.0, field.center().y);
    h.event(egui::Event::PointerMoved(from));
    h.step();
    h.event(egui::Event::PointerButton {
        pos: from,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    let steps = 8;
    for i in 1..=steps {
        let p = from + egui::vec2(dx * i as f32 / steps as f32, 0.0);
        h.event(egui::Event::PointerMoved(p));
        h.step();
    }
    let to = from + egui::vec2(dx, 0.0);
    h.event(egui::Event::PointerButton {
        pos: to,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    h.event(egui::Event::PointerGone);
    h.run_steps(3);
}

/// The owner's report: on the metres bunny, the split point's field
/// dragged 0.5 m per point of drag (a fixed `speed(0.5)` in document
/// units). Scrubbing its label now steps by a `nice_step` a few points on
/// screen at this zoom: millimetres, and the plane stays in the model.
#[test]
fn dragging_the_split_point_on_the_metre_bunny_moves_it_by_millimetres() {
    require_gpu!("dragging_the_split_point_on_the_metre_bunny_moves_it_by_millimetres");
    let Some(mut h) =
        bunny_in_metres("dragging_the_split_point_on_the_metre_bunny_moves_it_by_millimetres")
    else {
        return;
    };
    h.get_by_label("Split").click();
    settle(&mut h);
    let before = h.state().split.point();
    let step = mf_desktop::widgets::length_field::scrub_step(&h.state().scale_ctx());
    assert!(
        step.mm() >= 0.1 && step.mm() <= 10.0,
        "the scrub step on the framed bunny is millimetres: {step:?}"
    );
    scrub(&mut h, "Split point Z", 16.0);
    let after = h.state().split.point();
    let moved_mm = (after.z - before.z) * 1000.0;
    assert!(
        moved_mm > 0.0,
        "the drag moved the point: {before:?} -> {after:?}"
    );
    assert!(
        moved_mm <= 50.0,
        "16 points of drag moved the point {moved_mm} mm (0.5 m per point before the fix)"
    );
    assert_eq!(after.x, before.x);
    assert_eq!(after.y, before.y);
    // The field shows the new point with its unit, in the 1-2-5 step.
    assert!(
        shown(&h, "Split point Z").ends_with(" mm"),
        "{}",
        shown(&h, "Split point Z")
    );
}

/// Hollow's 2 mm default wall on a metres document reads "2.00 mm" (it
/// read "0" with two fixed decimals of metres).
#[test]
fn a_two_millimetre_wall_on_a_metres_document_shows_two_mm() {
    require_gpu!("a_two_millimetre_wall_on_a_metres_document_shows_two_mm");
    let mut h = cube_in(20.0, Unit::Meter);
    h.get_by_label("Hollow").click();
    settle(&mut h);
    let wall = h.state().hollow.wall.unwrap();
    assert!((wall.mm() - 2.0).abs() < 1e-12, "{wall:?}");
    assert_eq!(shown(&h, "Wall"), "2.00 mm");
}

/// A 0.6 mm Make solid cell on a metres document keeps its value frame
/// after frame (a 3-decimal slider of metres rounded it to 0.001 m), and
/// the field shows it.
#[test]
fn a_fine_make_solid_cell_on_a_metres_document_keeps_its_value() {
    require_gpu!("a_fine_make_solid_cell_on_a_metres_document_keeps_its_value");
    let mut h = cube_in(100.0, Unit::Meter);
    h.get_by_label("Make solid").click();
    settle(&mut h);
    let cell = Length::new(0.0006, Unit::Meter);
    h.state_mut()
        .tool_state_mut::<mf_desktop::shape_tools::SolidState>()
        .cell = Some(cell);
    h.run_steps(10);
    assert_eq!(
        h.state()
            .tool_state::<mf_desktop::shape_tools::SolidState>()
            .cell,
        Some(cell)
    );
    assert_eq!(shown(&h, "Cell"), "0.600 mm");
}

/// Inspect's size, area and volume carry their unit, through the one
/// formatter: a 20 mm cube on a metres document reads in millimetres,
/// never "0.0000" with no unit.
#[test]
fn inspect_shows_lengths_with_a_unit() {
    require_gpu!("inspect_shows_lengths_with_a_unit");
    let mut h = cube_in(20.0, Unit::Meter);
    h.get_by_label("Inspect").click();
    settle(&mut h);
    h.get_by_label("12 triangles · 20.0 mm × 20.0 mm × 20.0 mm");
    let d = h.state().current_analysis().unwrap().diagnostics.clone();
    let area = mf_desktop::defects::area_text(d.surface_area, Unit::Meter);
    let volume = mf_desktop::defects::volume_text(d.signed_volume, Unit::Meter);
    assert_eq!(area, "2400 mm\u{b2}");
    assert_eq!(volume, "8000 mm\u{b3}");
}

/// The defaults one model scale at a time: what each panel starts with,
/// in millimetres.
struct Defaults {
    diag_mm: f64,
    cell_mm: f64,
    wall_mm: f64,
    bound_mm: f64,
    box_mm: [f64; 3],
    step_mm: f64,
}

fn defaults(h: &mut Harness<'_, MeshFixerApp>) -> Defaults {
    let doc = h.state().session.document().units();
    let mm = |v: f64| Length::new(v, doc).mm();
    let diag_mm = mm(h.state().scale_ctx().bbox_diag);
    let step_mm = mf_desktop::widgets::length_field::scrub_step(&h.state().scale_ctx()).mm();
    h.get_by_label("Make solid").click();
    settle(h);
    let cell_mm = h
        .state()
        .tool_state::<mf_desktop::shape_tools::SolidState>()
        .cell
        .unwrap()
        .mm();
    h.get_by_label("Hollow").click();
    settle(h);
    let wall_mm = h.state().hollow.wall.unwrap().mm();
    h.get_by_label("Split").click();
    settle(h);
    let bound_mm = mf_desktop::shape_tools::bound_size(h.state()).0.mm();
    h.get_by_label("Print volume").click();
    settle(h);
    let size: DVec3 = h.state().split_flow.volume.size;
    let box_mm = [mm(size.x), mm(size.y), mm(size.z)];
    Defaults {
        diag_mm,
        cell_mm,
        wall_mm,
        bound_mm,
        box_mm,
        step_mm,
    }
}

fn assert_sane(what: &str, d: &Defaults) {
    let close = |a: f64, b: f64| (a - b).abs() <= 1e-6 * b.abs().max(1.0);
    assert!(
        close(d.cell_mm, d.diag_mm / 256.0),
        "{what}: cell {} of diagonal {}",
        d.cell_mm,
        d.diag_mm
    );
    assert!(close(d.wall_mm, 2.0), "{what}: wall {} mm", d.wall_mm);
    assert!(
        close(d.bound_mm, d.diag_mm / 2.0),
        "{what}: bound {} of diagonal {}",
        d.bound_mm,
        d.diag_mm
    );
    let profile = mf_parts::MachineProfile::uncalibrated().print_volume_mm;
    for k in 0..3 {
        assert!(
            close(d.box_mm[k], profile[k]),
            "{what}: box {:?} vs the profile's {profile:?}",
            d.box_mm
        );
    }
    // A scrub step is a small fraction of the framed model, never larger
    // than it, nor finer than it can show.
    assert!(
        d.step_mm < d.diag_mm / 10.0 && d.step_mm > d.diag_mm / 10_000.0,
        "{what}: scrub step {} mm on a {} mm model",
        d.step_mm,
        d.diag_mm
    );
}

#[test]
fn defaults_are_sane_on_a_ten_millimetre_part() {
    require_gpu!("defaults_are_sane_on_a_ten_millimetre_part");
    let mut h = cube_in(10.0, Unit::Millimeter);
    let d = defaults(&mut h);
    assert_sane("10 mm part", &d);
}

#[test]
fn defaults_are_sane_on_the_metre_bunny() {
    require_gpu!("defaults_are_sane_on_the_metre_bunny");
    let Some(mut h) = bunny_in_metres("defaults_are_sane_on_the_metre_bunny") else {
        return;
    };
    let d = defaults(&mut h);
    assert!(d.diag_mm > 150.0 && d.diag_mm < 300.0, "{}", d.diag_mm);
    assert_sane("0.156 m bunny", &d);
}

#[test]
fn defaults_are_sane_on_a_three_metre_part() {
    require_gpu!("defaults_are_sane_on_a_three_metre_part");
    let mut h = cube_in(3000.0, Unit::Meter);
    let d = defaults(&mut h);
    assert_sane("3 m part", &d);
}
