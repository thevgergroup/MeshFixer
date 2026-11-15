//! `Unit`, `nice_step` and `ScaleCtx` (consolidation plan C03).

use mf_units::{DocLength, Length, ScaleCtx, Unit, nice_step};

#[test]
fn mm_per_matches_the_one_conversion_table() {
    assert_eq!(Unit::Micron.mm_per(), 0.001);
    assert_eq!(Unit::Millimeter.mm_per(), 1.0);
    assert_eq!(Unit::Centimeter.mm_per(), 10.0);
    assert_eq!(Unit::Meter.mm_per(), 1000.0);
    assert!((Unit::Inch.mm_per() - 25.4).abs() < 1e-12);
    assert!((Unit::Foot.mm_per() - 304.8).abs() < 1e-12);
}

#[test]
fn symbol_and_name_cover_every_unit() {
    for unit in Unit::ALL {
        assert!(!unit.symbol().is_empty());
        assert!(!unit.name().is_empty());
    }
}

#[test]
fn parse_symbol_round_trips_every_symbol_it_prints() {
    for unit in Unit::ALL {
        // µm's canonical symbol uses the Unicode "µ", which `parse_symbol`
        // also accepts (alongside the ASCII "um" alias).
        assert_eq!(Unit::parse_symbol(unit.symbol()), Some(unit));
    }
    assert_eq!(Unit::parse_symbol("um"), Some(Unit::Micron));
    assert_eq!(Unit::parse_symbol("\""), Some(Unit::Inch));
    assert_eq!(Unit::parse_symbol("'"), Some(Unit::Foot));
    assert_eq!(Unit::parse_symbol("nonsense"), None);
}

#[test]
fn all_has_no_duplicates() {
    let mut seen = Unit::ALL.to_vec();
    seen.sort_by_key(|u| u.symbol());
    seen.dedup();
    assert_eq!(seen.len(), Unit::ALL.len());
}

#[test]
fn length_round_trips_through_any_unit() {
    let original = Length::new(12.5, Unit::Inch);
    for unit in Unit::ALL {
        let converted = original.to(unit);
        let back = converted.to(Unit::Inch);
        assert!(
            (back.value - original.value).abs() < 1e-9,
            "round-trip through {unit:?} lost precision: {back:?}"
        );
    }
}

#[test]
fn doc_length_round_trips() {
    let doc: DocLength = 42.0.into();
    let length = doc.length(Unit::Meter);
    assert_eq!(length.unit, Unit::Meter);
    assert_eq!(length.value, 42.0);
    let back: f64 = doc.into();
    assert_eq!(back, 42.0);
}

#[test]
fn nice_step_is_from_the_1_2_5_sequence() {
    let allowed = [
        0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 20.0, 50.0,
        100.0, 200.0, 500.0, 1000.0, 5000.0,
    ];
    for world_per_pt in [0.001, 0.01, 0.1, 1.0, 10.0, 100.0] {
        let step = nice_step(world_per_pt, 8.0, Unit::Millimeter);
        assert!(
            allowed.contains(&step.value),
            "{world_per_pt} mm/pt produced {step:?}, not in the 1-2-5 sequence"
        );
    }
}

#[test]
fn nice_step_clears_the_minimum_screen_size() {
    for world_per_pt in [0.001, 0.01, 0.1, 1.0, 10.0, 100.0, 1000.0] {
        let step = nice_step(world_per_pt, 8.0, Unit::Millimeter);
        let px = step.mm() / world_per_pt;
        assert!(
            px >= 8.0 - 1e-6 || step.value == 5000.0,
            "world_per_pt {world_per_pt}: step {step:?} only clears {px} pt"
        );
    }
}

/// Consolidation plan C09: every snap step is `nice_step(world_per_pt, 8
/// pt, display)`, at any zoom (1e-3 to 1e3 document units per screen
/// point — a model so close one point spans a thousandth of a unit, to
/// one so far out a point spans a thousand), on an mm, m or in document.
/// The step must always be from the 1-2-5 sequence and clear 8pt on
/// screen (the "a fixed 1-document-unit grid snaps every drag to zero on
/// a metre-scale model" bug, MF-OBJ-002, generalised across every scale
/// and unit rather than pinned to one document).
#[test]
fn snap_step_is_8pt_at_any_zoom() {
    let zooms = [1e-3, 1e-2, 1e-1, 1.0, 1e1, 1e2, 1e3];
    let docs = [Unit::Millimeter, Unit::Meter, Unit::Inch];
    for doc in docs {
        for &doc_units_per_pt in &zooms {
            let ctx = ScaleCtx {
                doc,
                display: doc,
                bbox_diag: 1.0,
                world_per_pt: Some(doc_units_per_pt),
            };
            let step = nice_step(ctx.mm_per_pt(), 8.0, ctx.display);
            let px = step.mm() / ctx.mm_per_pt();
            assert!(
                px >= 8.0 - 1e-6 || step.mm() == 5000.0,
                "{doc:?} at {doc_units_per_pt} doc units/pt: step {step:?} only clears {px} pt"
            );
            let allowed = [
                0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 20.0,
                50.0, 100.0, 200.0, 500.0, 1000.0, 5000.0,
            ];
            let step_mm = step.mm();
            assert!(
                allowed
                    .iter()
                    .any(|&a| (a - step_mm).abs() < step_mm * 1e-9 + 1e-12),
                "{doc:?} at {doc_units_per_pt} doc units/pt: step {step:?} not in the 1-2-5 sequence"
            );
        }
    }
}

#[test]
fn nice_step_respects_the_display_unit() {
    let step_mm = nice_step(1.0, 8.0, Unit::Millimeter);
    let step_m = nice_step(1.0, 8.0, Unit::Meter);
    assert!((step_mm.mm() - step_m.mm()).abs() < 1e-9);
    assert_eq!(step_mm.unit, Unit::Millimeter);
    assert_eq!(step_m.unit, Unit::Meter);
}

#[test]
fn scale_ctx_mm_per_pt_uses_world_per_pt_when_present() {
    let ctx = ScaleCtx {
        doc: Unit::Meter,
        display: Unit::Meter,
        bbox_diag: 1.0,
        world_per_pt: Some(0.002),
    };
    // 0.002 document units (metres) per pt = 2 mm per pt.
    assert!((ctx.mm_per_pt() - 2.0).abs() < 1e-9);
}

#[test]
fn scale_ctx_falls_back_to_doc_unit_with_no_camera() {
    let ctx = ScaleCtx::new(Unit::Meter);
    assert_eq!(ctx.doc, Unit::Meter);
    assert_eq!(ctx.display, Unit::Meter);
    assert!(ctx.world_per_pt.is_none());
    assert!((ctx.mm_per_pt() - 1000.0).abs() < 1e-9);
}
