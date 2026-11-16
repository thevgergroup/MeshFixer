#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Owner-reported unit bug (consolidation plan C01, units-audit.md
//! mf-desktop/src/app.rs:6707-6754 `summarize_value`): the History
//! panel's Details line labels a recorded (already doc-unit) parameter
//! value with its *spec* unit ("mm") rather than the *document's* unit.
//! On a metres document, Hollow's 2 mm wall thickness is recorded as
//! 0.002 doc units, and Details prints "0.002 mm" instead of "2 mm" or
//! "2.00 mm". Fixed by C11 ("History and status through
//! `format_length`").

use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mf_desktop::{MeshFixerApp, Settings};
use mf_geometry::{ContentHash, Mesh};
use mf_io::threemf::{ModelObject, Unit};
use mf_io::{Format, ImportedModel, SourceInfo, SourcePrecision};

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

fn at(t: [f64; 3]) -> [f64; 12] {
    [
        1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, t[0], t[1], t[2],
    ]
}

/// As `history_labels.rs::opened`, but at `unit` instead of always
/// millimetres, so a metres document can be driven without the import
/// prompt (`core_workflows.rs::opened_with_units` does the same thing).
fn opened_at(objects: Vec<(&str, Mesh, [f64; 12])>, unit: Unit) -> Harness<'static, MeshFixerApp> {
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
        unit: Some(unit),
        source: SourceInfo {
            path: "unit-bugs.3mf".into(),
            content_hash: ContentHash([0; 32]),
            format: Format::ThreeMf,
            precision: SourcePrecision::F32,
            exporter_hint: None,
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

fn panel_button<'a>(h: &'a Harness<'_, MeshFixerApp>, label: &'a str) -> egui_kittest::Node<'a> {
    h.query_all_by_label(label)
        .max_by(|a, b| a.rect().width().total_cmp(&b.rect().width()))
        .unwrap_or_else(|| panic!("no {label:?}"))
}

/// The current path's most recent step's Details text.
fn last_details(h: &Harness<'_, MeshFixerApp>) -> String {
    h.state()
        .breadcrumb()
        .into_iter()
        .rfind(|(_, _, _, reached)| *reached)
        .map(|(_, _, details, _)| details)
        .expect("a step was taken")
}

/// Hollow a cube at the default 2 mm wall thickness, on a *metres*
/// document: History's Details line must read "2 mm" (or "2.00 mm"),
/// the physically correct label, never "0.002 mm" (the recorded
/// doc-unit value, 0.002, relabelled with the spec's literal "mm").
#[test]
fn history_details_for_a_hollow_wall_on_a_metres_document_reads_physical_millimetres() {
    require_gpu!(
        "history_details_for_a_hollow_wall_on_a_metres_document_reads_physical_millimetres"
    );
    // A 20 mm cube, modelled at 0.02 doc units in a metres document.
    let mut h = opened_at(
        vec![("cube", mf_fixtures::cube(0.02), at([0.0; 3]))],
        Unit::Meter,
    );
    assert_eq!(h.state().session.document().units(), Unit::Meter);
    h.get_by_label("Hollow").click();
    settle(&mut h);
    // Leave wall thickness at its default (2 mm == 0.002 doc units).
    panel_button(&h, "Hollow").click();
    wait_until(&mut h, "the hollow", |a| a.busy.is_none());
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    let details = last_details(&h);
    assert!(
        details.contains("2 mm") || details.contains("2.00 mm") || details.contains("2.0 mm"),
        "History Details for a 2 mm wall on a metres document should \
         read a physical \"2 mm\", not the raw recorded doc-unit value \
         labelled \"mm\": got {details:?}"
    );
    assert!(
        !details.contains("0.002 mm"),
        "History Details showed the owner's exact bug, the doc-unit \
         value 0.002 relabelled \"mm\": {details:?}"
    );
}

/// As the test above, but across the three document units the owner's
/// bug class spans (consolidation plan C11's own enforcement test,
/// `history_details_use_document_unit`): millimetres, metres and
/// inches. A physically 20 mm cube's default Hollow wall (2 mm) must
/// always read in the *document's* unit, through `mf_units::format_length`,
/// never the spec's own literal "mm" relabelling a doc-unit float — the
/// bug is the same on every unit, not only metres (a millimetres
/// document happens to make the recorded value and the physical value
/// numerically equal, 2.0 == 2.0, so it alone would never catch a
/// mislabelling bug; metres and inches do, since the recorded value and
/// the millimetre figure then differ).
#[test]
fn history_details_use_document_unit() {
    require_gpu!("history_details_use_document_unit");
    // A 20 mm cube's side length, expressed in each document's own
    // unit, so every case models the same physical cube. The wall
    // thickness is set to a physical 3 mm, off the spec's 2 mm default
    // (so History Details always shows it: `history_details_of` hides
    // any parameter left at its default). `forbidden` is what the
    // owner's bug would have printed: the recorded doc-unit wall value
    // (3 mm converted into `unit`), relabelled with the spec's own
    // literal "mm" rather than converted and labelled with `unit`. On a
    // millimetres document that recorded value is 3.0, the same number
    // the correct "3.00 mm" uses, so it alone could never show the bug;
    // metres and inches can, because the recorded value and the correct
    // millimetre figure then differ.
    let side_in = |unit: Unit| 20.0 / unit.mm_per();
    let wall_in = |unit: Unit| 3.0 / unit.mm_per();
    for (unit, forbidden) in [
        (Unit::Millimeter, "0.00300 mm"),
        (Unit::Meter, "0.003 mm"),
        (Unit::Inch, "0.118 mm"),
    ] {
        let mut h = opened_at(
            vec![("cube", mf_fixtures::cube(side_in(unit)), at([0.0; 3]))],
            unit,
        );
        assert_eq!(h.state().session.document().units(), unit);
        h.get_by_label("Hollow").click();
        settle(&mut h);
        // A physical 3 mm wall, off the spec's 2 mm default, in this
        // document's own unit.
        h.state_mut().hollow.wall = Some(mf_units::Length::new(wall_in(unit), unit));
        panel_button(&h, "Hollow").click();
        wait_until(&mut h, "the hollow", |a| a.busy.is_none());
        settle(&mut h);
        assert!(
            h.state().refusal.is_none(),
            "{unit:?}: {:?}",
            h.state().refusal
        );
        let details = last_details(&h);
        // The recorded doc-unit value, mislabelled with the spec's own
        // "mm" instead of converted and labelled with the document's
        // unit, never appears (the owner's exact bug, generalised to
        // every unit, not only metres).
        assert!(
            !details.contains(forbidden),
            "{unit:?}: History Details showed a doc-unit value \
             mislabelled \"mm\" instead of the document's own unit: \
             got {details:?}"
        );
        // And the physically correct "2 mm" wall reads in the
        // document's own unit symbol, not a bare number with no unit
        // or the wrong one.
        let symbol = unit.symbol();
        assert!(
            details.contains(symbol),
            "{unit:?}: History Details for the wall thickness should \
             show the document's own unit {symbol:?}: got {details:?}"
        );
    }
}

/// Inspect's Units control (import-units-brief.md, "Fix it afterwards",
/// consolidation plan C15): "Reinterpret" relabels the document's unit
/// with no object changed, "Scale" changes every object's physical
/// size with the document's own unit unchanged. Each runs its recorded
/// command (`document.reinterpret_units` / `document.scale_units`,
/// C07) and round-trips with undo.
#[test]
fn inspect_units_reinterpret_and_scale_round_trip_with_undo() {
    require_gpu!("inspect_units_reinterpret_and_scale_round_trip_with_undo");
    // A 10 mm cube in a millimetres document.
    let mut h = opened_at(
        vec![("cube", mf_fixtures::cube(10.0), at([0.0; 3]))],
        Unit::Millimeter,
    );
    assert_eq!(h.state().session.document().units(), Unit::Millimeter);
    let part = h.state().session.document().objects()[0].id;
    let original_transform = h.state().session.document().object(part).unwrap().transform;
    let original_hash = h
        .state()
        .session
        .document()
        .object(part)
        .unwrap()
        .geometry
        .content_hash();

    h.get_by_label("Inspect").click();
    settle(&mut h);

    // Reinterpret to metres: the document's unit changes, the object's
    // own coordinates and transform don't.
    h.state_mut().inspect_units = Some(Unit::Meter);
    settle(&mut h);
    let history = h.state().session.history_labels().len();
    panel_button(&h, "Reinterpret").click();
    wait_until(&mut h, "reinterpret", |a| {
        a.busy.is_none() && a.session.history_labels().len() > history
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    assert_eq!(h.state().session.document().units(), Unit::Meter);
    assert_eq!(
        h.state().session.document().object(part).unwrap().transform,
        original_transform,
        "reinterpret must not touch the transform"
    );
    assert_eq!(
        h.state()
            .session
            .document()
            .object(part)
            .unwrap()
            .geometry
            .content_hash(),
        original_hash,
        "reinterpret must not touch the mesh"
    );

    assert!(h.state_mut().session.undo());
    settle(&mut h);
    assert_eq!(
        h.state().session.document().units(),
        Unit::Millimeter,
        "undo relabels back"
    );

    assert!(h.state_mut().session.redo());
    settle(&mut h);
    assert_eq!(h.state().session.document().units(), Unit::Meter);

    // Scale to metres: the document keeps millimetres... no, stays
    // metres (reinterpret above already changed it); scale itself
    // never relabels, only resizes. Pick a different unit (inches) so
    // the ratio isn't 1 and a change is guaranteed.
    h.get_by_label("Inspect").click();
    settle(&mut h);
    h.state_mut().inspect_units = Some(Unit::Inch);
    settle(&mut h);
    let before_scale = h.state().session.document().object(part).unwrap().transform;
    let history = h.state().session.history_labels().len();
    panel_button(&h, "Scale").click();
    wait_until(&mut h, "scale", |a| {
        a.busy.is_none() && a.session.history_labels().len() > history
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    let after_scale = h.state().session.document().object(part).unwrap().transform;
    assert_eq!(
        h.state().session.document().units(),
        Unit::Meter,
        "scale never relabels the document"
    );
    // 1 inch in metres (0.0254) is the ratio every transform component
    // scales by.
    for k in 0..12 {
        assert!(
            (after_scale[k] - before_scale[k] * 0.0254).abs() < 1e-9,
            "index {k}: {after_scale:?} vs {before_scale:?} * 0.0254"
        );
    }

    assert!(h.state_mut().session.undo());
    settle(&mut h);
    assert_eq!(
        h.state().session.document().object(part).unwrap().transform,
        before_scale,
        "undo restores the pre-scale transform exactly"
    );

    assert!(h.state_mut().session.redo());
    settle(&mut h);
    assert_eq!(
        h.state().session.document().object(part).unwrap().transform,
        after_scale
    );
}
