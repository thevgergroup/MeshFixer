//! `format_length`, `format_area` and `format_volume` (ADR-0006 rule 4):
//! the one formatter for every length, area and volume shown to a
//! person — fields, the status line, refusals, History and Inspect. All
//! three show 3 significant figures, pick an automatic display unit,
//! and never print "0.000" for a value that isn't genuinely zero (the
//! owner's bug: Make solid's refusal said "at least 0.000" for a
//! sub-millimetre cell size on a metre document, because the old
//! formatter rounded to a fixed number of decimals of document units
//! with no sense of how small "small" was in the shown unit).

use crate::length::Length;
use crate::unit::Unit;

/// Formats `v` to 3 significant figures, choosing an automatic display
/// unit (starting from `unit`, [`Unit::auto_for`]) so the value reads
/// comfortably — "12.3 mm" rather than "0.0123 m", and never "0.000" for
/// a value that isn't exactly zero. `v` is taken to already be in
/// `unit`.
pub fn format_length(v: f64, unit: Unit) -> String {
    let length = Length::new(v, unit);
    let display = auto_display_unit(length);
    let shown = length.to(display);
    format!(
        "{} {}",
        significant_figures(shown.value, 3),
        display.symbol()
    )
}

/// As [`format_length`], for an area already in `unit²`. 3 significant
/// figures, with the unit written as `mm²`, `m²`, and so on.
pub fn format_area(v: f64, unit: Unit) -> String {
    // Areas scale with the square of length, so pick the display unit
    // from the equivalent *side length* (its square root) rather than
    // the raw area — otherwise a 4 m² area (a 2 m square) would compare
    // its raw "4" against the length auto-unit thresholds meant for
    // lengths, not areas, and misjudge the right unit.
    let side = Length::new(v.abs().sqrt(), unit);
    let display = auto_display_unit(side);
    let factor = (unit.mm_per() / display.mm_per()).powi(2);
    let shown = v * factor;
    format!(
        "{} {}\u{b2}",
        significant_figures(shown, 3),
        display.symbol()
    )
}

/// As [`format_length`], for a volume already in `unit³`. 3 significant
/// figures, with the unit written as `mm³`, `m³`, and so on.
pub fn format_volume(v: f64, unit: Unit) -> String {
    let side = Length::new(v.abs().cbrt(), unit);
    let display = auto_display_unit(side);
    let factor = (unit.mm_per() / display.mm_per()).powi(3);
    let shown = v * factor;
    format!(
        "{} {}\u{b3}",
        significant_figures(shown, 3),
        display.symbol()
    )
}

/// Picks the best display unit for `length`, staying within the same
/// measurement system (metric stays metric, imperial stays imperial) and
/// switching to the next smaller unit in that system once the value
/// would otherwise show below 0.1 of the current display unit (ADR-0006
/// §6.5: "m → mm, ft → in" — metres step straight to millimetres,
/// skipping centimetres, which is only ever the *starting* display unit
/// when a centimetre document or value is formatted directly), so a 2
/// mm wall on a metre document shows "2 mm", not "0.002 m".
fn auto_display_unit(length: Length) -> Unit {
    let mm = length.mm().abs();
    // Centimetre is deliberately absent from the step-down ladder (only
    // reachable as the starting unit, never a landing spot stepped
    // into): the ladder metres fall through to is mm then µm.
    let metric_order = [Unit::Meter, Unit::Millimeter, Unit::Micron];
    let imperial_order = [Unit::Foot, Unit::Inch];
    let order: &[Unit] = if matches!(length.unit, Unit::Inch | Unit::Foot) {
        &imperial_order
    } else {
        &metric_order
    };
    if length.unit == Unit::Centimeter && mm / Unit::Centimeter.mm_per() >= 0.1 {
        return Unit::Centimeter;
    }
    // Starting from the document/original unit's position in its
    // system (centimetre, if that's the original unit, steps down into
    // the same ladder as metres, starting from the top), step to
    // smaller units while the value is under 0.1 of the current
    // candidate; stop at the smallest unit in that system either way.
    // Indexed, not `Iterator::position`, because the starting index is
    // already known by construction (0 for an unlisted unit such as
    // centimetre, or the matching entry otherwise), so there is no
    // "not found" case to paper over with a fallback.
    // allow-swallow: any unit not in `order` (only centimetre) falls back to index 0, the ladder's largest unit, by design
    let mut index = order.iter().position(|&u| u == length.unit).unwrap_or(0);
    loop {
        let candidate = order[index];
        let in_candidate = mm / candidate.mm_per();
        if in_candidate == 0.0 || in_candidate.abs() >= 0.1 || index + 1 >= order.len() {
            return candidate;
        }
        index += 1;
    }
}

/// Rounds `v` to `sig` significant figures and formats it with exactly
/// the decimals needed to show them: whole numbers whose magnitude
/// already meets `sig` digits show no decimals ("123"), smaller ones
/// show enough decimals to keep `sig` significant figures, including
/// trailing zeros ("0.500", not "0.5" — every digit up to `sig` is
/// shown, matching Fusion/Onshape-style fields where a value's
/// precision is part of what's displayed). Never collapses a nonzero,
/// finite value to "0" or "0.000": if the decimal count implied by
/// `sig` would still round a genuinely nonzero value down to zero
/// (rounding to the computed `decimals` landed exactly on a
/// power-of-ten boundary, e.g. 0.0999996 at 3 s.f.), one more decimal
/// is added until a nonzero digit shows, bounded so pathological tiny
/// values still terminate. A non-finite `v` (NaN, infinity) formats as
/// Rust's own `{v}` would ("NaN", "inf"); this is a display formatter,
/// not a parser, so it has no error path — callers that can produce
/// non-finite values from user input reject them before formatting, at
/// the parser (`parse_length` never returns one).
///
/// Unit-independent: this is the shared numeric core behind
/// [`format_length`] (which also picks a display unit) and behind
/// `mf_spec::format_length`'s delegation, which has no unit to pick
/// from (D20: operations work purely in document units with no `Unit`
/// in scope) and so calls this directly.
pub fn significant_figures(v: f64, sig: i32) -> String {
    if !v.is_finite() {
        return format!("{v}");
    }
    if v == 0.0 {
        return "0".to_owned();
    }
    let a = v.abs();
    // The power of ten of the leading digit: for 12.3, magnitude 1 (10^1
    // <= 12.3 < 10^2); for 0.0123, magnitude -2.
    let magnitude = a.log10().floor() as i32;
    let mut decimals = (sig - 1 - magnitude).max(0);
    let rounded = loop {
        let scale = 10f64.powi(decimals);
        let rounded = (v * scale).round() / scale;
        if rounded != 0.0 || decimals >= 15 {
            break rounded;
        }
        decimals += 1;
    };
    format!("{rounded:.*}", decimals as usize)
}
