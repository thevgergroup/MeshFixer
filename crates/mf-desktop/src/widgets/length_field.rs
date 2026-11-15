//! The one length field (ADR-0006 rule 1/1b, units-audit.md §6.5,
//! consolidation plan C13): a text field that accepts a typed length or
//! expression ("10 in", "5 cm + 2 mm", "1/8\"", a bare number meaning the
//! display unit) through [`mf_units::parse_length`], and shows its value
//! through [`mf_units::format_length`].
//!
//! - On Enter or focus loss the text is parsed. A parse error is shown
//!   inline and the previous value is kept (never replaced by 0).
//! - A parsed value is clamped to the field's range, then redisplayed in
//!   the display unit.
//! - Dragging the label scrubs the value in [`mf_units::nice_step`]
//!   increments.
//!
//! [`length_slider`] is the same field next to a logarithmic slider whose
//! range comes from the [`ScaleCtx`] (the model's size).
//!
//! The field holds a [`Length`], never a bare number: what goes on to an
//! [`crate::intent::Intent`] is exactly what the person typed, and only
//! `Session::prepare` converts it to document units.

use eframe::egui;
use mf_units::{Length, ParseError, ScaleCtx, Unit, format_length, nice_step, parse_length};

use crate::theme;

/// How a [`length_field`] reads and shows its value.
#[derive(Debug, Clone, Copy)]
pub struct LengthFieldOpts<'a> {
    /// The smallest and largest value the field accepts; a typed value
    /// outside is clamped to it.
    pub range: (Length, Length),
    /// The document's scale: its display unit (what a bare number means
    /// and what the value is shown in) and the zoom the scrub step comes
    /// from.
    pub scale: &'a ScaleCtx,
    /// The field's label (also its accessible name).
    pub label: &'a str,
}

/// The field's remembered state between frames.
#[derive(Debug, Clone, Default)]
struct FieldState {
    /// The text being edited (or last shown).
    text: String,
    /// What `text` showed when it was last written from the value, so an
    /// untouched field isn't re-parsed (3 significant figures would round
    /// a more precise value away).
    shown: String,
    /// The last parse error, shown under the field until the next edit.
    error: Option<String>,
    /// Label-drag distance not yet turned into a whole step, in points.
    scrub: f32,
}

/// The text box's width, in points (fits "1234.5 mm").
const FIELD_WIDTH_PT: f32 = 84.0;
/// Points of label drag per scrub step.
const SCRUB_PT_PER_STEP: f32 = 4.0;
/// The smallest on-screen size of a scrub step, in points.
const STEP_MIN_PT: f64 = 8.0;

/// `value` as the field shows it: [`format_length`] in the display unit.
pub fn display_text(value: Length, scale: &ScaleCtx) -> String {
    format_length(value.to(scale.display).value, scale.display)
}

/// Parses `text` as the field would on Enter: a length in
/// `opts.scale.display` (for a bare number), clamped to `opts.range`.
/// An error leaves the caller's value untouched.
pub fn commit_text(text: &str, opts: &LengthFieldOpts<'_>) -> Result<Length, ParseError> {
    let parsed = parse_length(text, opts.scale.display)?;
    Ok(clamp(parsed, opts.range, opts.scale.display))
}

fn clamp(v: Length, (lo, hi): (Length, Length), display: Unit) -> Length {
    let mm = v.mm().clamp(lo.mm().min(hi.mm()), hi.mm().max(lo.mm()));
    if mm == v.mm() {
        v.to(display)
    } else {
        Length::new(mm, Unit::Millimeter).to(display)
    }
}

/// The scrub step for `scale`: a 1-2-5 step at least [`STEP_MIN_PT`] on
/// screen at the current zoom (or at the document scale with no view).
pub fn scrub_step(scale: &ScaleCtx) -> Length {
    nice_step(scale.mm_per_pt(), STEP_MIN_PT, scale.display)
}

/// A length field; see the module docs. Returns the field's response,
/// `changed()` only when the value was committed this frame (Enter, focus
/// loss or a label scrub), never on a mere keystroke.
pub fn length_field(
    ui: &mut egui::Ui,
    id: egui::Id,
    value: &mut Length,
    opts: &LengthFieldOpts<'_>,
) -> egui::Response {
    let state_id = id.with("length_field");
    let mut state: FieldState = ui.data(|d| d.get_temp(state_id)).unwrap_or_default(); // allow-swallow: no state yet is a fresh field
    let edit_id = id.with("text");
    let focused = ui.memory(|m| m.has_focus(edit_id));
    let current = display_text(*value, opts.scale);
    // Not being edited and not showing an error: follow the value (it
    // may have changed from elsewhere, or be shown for the first time).
    if !focused && state.error.is_none() && state.text != current {
        state.text = current.clone();
        state.shown = current.clone();
    }
    let mut committed = false;
    let inner = ui.vertical(|ui| {
        let row = ui.horizontal(|ui| {
            let label = ui.add(egui::Label::new(opts.label).sense(egui::Sense::drag()));
            let label = label.on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
            if label.dragged() {
                state.scrub += label.drag_delta().x;
                let steps = (state.scrub / SCRUB_PT_PER_STEP).trunc();
                if steps != 0.0 {
                    state.scrub -= steps * SCRUB_PT_PER_STEP;
                    let step = scrub_step(opts.scale);
                    let next =
                        Length::new(value.mm() + f64::from(steps) * step.mm(), Unit::Millimeter);
                    *value = clamp(next, opts.range, opts.scale.display);
                    state.error = None;
                    state.text = display_text(*value, opts.scale);
                    state.shown = state.text.clone();
                    committed = true;
                }
            } else {
                state.scrub = 0.0;
            }
            let edit = ui.add(
                egui::TextEdit::singleline(&mut state.text)
                    .id(edit_id)
                    .desired_width(FIELD_WIDTH_PT),
            );
            let edit = edit.labelled_by(label.id);
            if edit.changed() {
                state.error = None;
            }
            if edit.lost_focus() && state.text != state.shown {
                match commit_text(&state.text, opts) {
                    Ok(v) => {
                        *value = v;
                        state.error = None;
                        state.text = display_text(v, opts.scale);
                        state.shown = state.text.clone();
                        committed = true;
                    }
                    Err(e) => state.error = Some(e.to_string()),
                }
            }
        });
        if let Some(err) = &state.error {
            ui.label(
                egui::RichText::new(err)
                    .size(theme::TYPE_CAPTION)
                    .color(ui.visuals().error_fg_color),
            );
        }
        row.response
    });
    ui.data_mut(|d| d.insert_temp(state_id, state));
    // The row's own response, not the text edit's: a keystroke changes
    // the text, but only a commit changes the value.
    let mut response = inner.inner;
    if committed {
        response.mark_changed();
    }
    response
}

/// A [`length_field`] with a logarithmic slider beside it. The slider's
/// range is the model's size from `opts.scale` (a thousandth of the
/// bounding-box diagonal up to the whole diagonal), intersected with
/// `opts.range`; with no model (a zero diagonal) only the field shows.
pub fn length_slider(
    ui: &mut egui::Ui,
    id: egui::Id,
    value: &mut Length,
    opts: &LengthFieldOpts<'_>,
) -> egui::Response {
    let scale = opts.scale;
    let display = scale.display;
    let diag = Length::new(scale.bbox_diag, scale.doc);
    let mut changed = false;
    if diag.mm() > 0.0 {
        let lo = (diag.mm() / 1000.0).max(opts.range.0.mm());
        let hi = diag.mm().min(opts.range.1.mm());
        if lo < hi {
            let to_display = |mm: f64| Length::new(mm, Unit::Millimeter).to(display).value;
            let mut v = value.to(display).value;
            let slider = ui.add(
                egui::Slider::new(&mut v, to_display(lo)..=to_display(hi))
                    .logarithmic(true)
                    .show_value(false),
            );
            if slider.changed() {
                *value = Length::new(v, display);
                changed = true;
            }
        }
    }
    let mut response = length_field(ui, id, value, opts);
    if changed {
        response.mark_changed();
    }
    response
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests may panic on failure
    use super::*;

    fn opts(scale: &ScaleCtx) -> LengthFieldOpts<'_> {
        LengthFieldOpts {
            range: (
                Length::new(0.0, Unit::Millimeter),
                Length::new(10.0, Unit::Meter),
            ),
            scale,
            label: "Wall",
        }
    }

    #[test]
    fn expression_parses_in_any_unit() {
        let scale = ScaleCtx::new(Unit::Millimeter);
        let v = commit_text("5 cm + 2 mm", &opts(&scale)).unwrap();
        assert!((v.mm() - 52.0).abs() < 1e-9);
        assert_eq!(v.unit, Unit::Millimeter);
        assert_eq!(display_text(v, &scale), "52.0 mm");
    }

    #[test]
    fn bare_number_means_the_display_unit() {
        let mut scale = ScaleCtx::new(Unit::Millimeter);
        scale.display = Unit::Inch;
        let v = commit_text("2", &opts(&scale)).unwrap();
        assert!((v.mm() - 50.8).abs() < 1e-9);
    }

    #[test]
    fn invalid_input_is_an_error_not_zero() {
        let scale = ScaleCtx::new(Unit::Millimeter);
        assert!(commit_text("5 bananas", &opts(&scale)).is_err());
        assert!(commit_text("", &opts(&scale)).is_err());
        assert!(commit_text("in * in", &opts(&scale)).is_err());
    }

    #[test]
    fn out_of_range_clamps() {
        let scale = ScaleCtx::new(Unit::Millimeter);
        let v = commit_text("20 m", &opts(&scale)).unwrap();
        assert!((v.mm() - 10_000.0).abs() < 1e-9);
        let v = commit_text("-3 mm", &opts(&scale)).unwrap();
        assert_eq!(v.mm(), 0.0);
    }

    #[test]
    fn unit_round_trip_keeps_physical_size() {
        let scale = ScaleCtx::new(Unit::Meter);
        let v = commit_text("10 in", &opts(&scale)).unwrap();
        assert_eq!(v.unit, Unit::Meter);
        assert!((v.mm() - 254.0).abs() < 1e-9);
        // The displayed text parses back to the same length.
        let shown = display_text(v, &scale);
        let back = commit_text(&shown, &opts(&scale)).unwrap();
        assert!((back.mm() - 254.0).abs() < 1e-9, "{shown}");
    }
}
