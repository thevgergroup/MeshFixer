#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The shared length field (ADR-0006 rule 1b, consolidation plan C13),
//! driven through the real widget: typed expressions commit on Enter,
//! invalid input keeps the previous value with an inline error, and the
//! committed value is shown through `format_length` in the display unit.

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mf_desktop::widgets::{LengthFieldOpts, length_field};
use mf_units::{Length, ScaleCtx, Unit};

struct State {
    value: Length,
    scale: ScaleCtx,
    changed: usize,
}

fn harness(value: Length, scale: ScaleCtx) -> Harness<'static, State> {
    Harness::new_ui_state(
        |ui, s: &mut State| {
            let scale = s.scale;
            let opts = LengthFieldOpts {
                range: (
                    Length::new(0.0, Unit::Millimeter),
                    Length::new(10.0, Unit::Meter),
                ),
                scale: &scale,
                label: "Wall",
            };
            if length_field(ui, egui::Id::unique("wall"), &mut s.value, &opts).changed() {
                s.changed += 1;
            }
        },
        State {
            value,
            scale,
            changed: 0,
        },
    )
}

/// Replaces the field's text with `text` and presses Enter.
fn type_and_enter(h: &mut Harness<'_, State>, text: &str) {
    h.get_by_label("Wall").focus();
    h.run();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.run();
    h.get_by_label("Wall").type_text(text);
    h.run();
    h.key_press(egui::Key::Enter);
    h.run();
}

fn shown(h: &Harness<'_, State>) -> String {
    h.get_by_label("Wall").value().unwrap()
}

#[test]
fn expression_commits_and_shows_in_the_display_unit() {
    let mut h = harness(
        Length::new(2.0, Unit::Millimeter),
        ScaleCtx::new(Unit::Millimeter),
    );
    h.run();
    assert_eq!(shown(&h), "2.00 mm");
    type_and_enter(&mut h, "5 cm + 2 mm");
    assert!((h.state().value.mm() - 52.0).abs() < 1e-9);
    assert_eq!(h.state().changed, 1);
    assert_eq!(shown(&h), "52.0 mm");
}

#[test]
fn invalid_input_keeps_the_previous_value_and_says_why() {
    let mut h = harness(
        Length::new(2.0, Unit::Millimeter),
        ScaleCtx::new(Unit::Millimeter),
    );
    h.run();
    type_and_enter(&mut h, "5 bananas");
    assert_eq!(h.state().value, Length::new(2.0, Unit::Millimeter));
    assert_eq!(h.state().changed, 0);
    assert!(
        h.query_by_label_contains("is not a unit").is_some(),
        "an inline error should explain the bad input"
    );
    // A good value afterwards clears the error.
    type_and_enter(&mut h, "3");
    assert!((h.state().value.mm() - 3.0).abs() < 1e-9);
    assert!(h.query_by_label_contains("is not a unit").is_none());
}

#[test]
fn typed_units_round_trip_into_the_display_unit() {
    // A metre document: "10 in" stays 254 mm physically, shown in the
    // document's display unit (format_length picks a readable one).
    let mut h = harness(Length::new(0.1, Unit::Meter), ScaleCtx::new(Unit::Meter));
    h.run();
    type_and_enter(&mut h, "10 in");
    let v = h.state().value;
    assert_eq!(v.unit, Unit::Meter);
    assert!((v.mm() - 254.0).abs() < 1e-9);
    let text = shown(&h);
    let back = mf_units::parse_length(&text, Unit::Meter).unwrap();
    assert!((back.mm() - 254.0).abs() < 1e-6, "{text}");
}
