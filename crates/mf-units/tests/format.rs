//! `format_length`, `format_area`, `format_volume` across every unit and
//! the "never print 0.000 for a nonzero value" rule (consolidation plan
//! C03, ADR-0006 rule 4), plus `format_never_prints_zero_for_nonzero`, a
//! property test over many magnitudes.

use mf_units::{Unit, format_area, format_length, format_volume};

#[test]
fn three_significant_figures() {
    assert_eq!(format_length(123.456, Unit::Millimeter), "123 mm");
    assert_eq!(format_length(12.3456, Unit::Millimeter), "12.3 mm");
    assert_eq!(format_length(1.23456, Unit::Millimeter), "1.23 mm");
}

#[test]
fn switches_to_a_smaller_display_unit_below_a_tenth() {
    // A 2 mm wall on a metres document must read in millimetres, not
    // "0.00200 m" (the owner's bug this whole crate exists to fix) —
    // still 3 significant figures ("2.00 mm"), same as every other
    // value.
    let s = format_length(0.002, Unit::Meter);
    assert_eq!(s, "2.00 mm", "got {s:?}");
}

#[test]
fn stays_in_metres_when_comfortably_above_a_tenth() {
    let s = format_length(1.5, Unit::Meter);
    assert_eq!(s, "1.50 m");
}

#[test]
fn imperial_switches_feet_to_inches() {
    // 0.05 ft = 0.6 in: below a tenth of a foot, so it should read in
    // inches, staying within the imperial system (never jumping to mm).
    let s = format_length(0.05, Unit::Foot);
    assert!(s.ends_with("in"), "expected inches, got {s:?}");
    assert!(
        !s.contains("mm"),
        "imperial must not switch to metric: {s:?}"
    );
}

#[test]
fn never_prints_zero_point_zero_zero_zero_for_a_specific_small_value() {
    // The exact owner-reported bug: Make solid's refusal text for a
    // sub-millimetre cell size on a metres document used to read
    // "at least 0.000".
    let s = format_length(0.00025, Unit::Meter);
    assert_ne!(s, "0.000 m");
    assert!(!s.starts_with("0 "), "got {s:?}");
    assert!(!s.starts_with("0.000"), "got {s:?}");
}

#[test]
fn zero_itself_prints_as_zero() {
    let s = format_length(0.0, Unit::Millimeter);
    assert!(s.starts_with('0'), "got {s:?}");
}

#[test]
fn every_unit_formats() {
    for unit in Unit::ALL {
        let s = format_length(5.0, unit);
        assert!(!s.is_empty());
        assert_ne!(s, "0", "unit {unit:?} produced {s:?} for a nonzero value");
    }
}

#[test]
fn format_area_never_prints_zero_for_nonzero() {
    // A 2mm x 2mm face (4 mm^2) on a metres document is 4e-6 m^2.
    let s = format_area(4e-6, Unit::Meter);
    assert!(!s.starts_with("0.000"), "got {s:?}");
    assert!(s.contains('\u{b2}'), "expected a squared unit, got {s:?}");
}

#[test]
fn format_volume_never_prints_zero_for_nonzero() {
    let s = format_volume(8e-9, Unit::Meter);
    assert!(!s.starts_with("0.000"), "got {s:?}");
    assert!(s.contains('\u{b3}'), "expected a cubed unit, got {s:?}");
}

#[test]
fn format_area_and_volume_three_sig_figs_in_place() {
    // A 2m x 2m face stays readable in m^2, not switched to mm^2.
    let s = format_area(4.0, Unit::Meter);
    assert_eq!(s, "4.00 m\u{b2}");
    let s = format_volume(8.0, Unit::Meter);
    assert_eq!(s, "8.00 m\u{b3}");
}

/// `format_never_prints_zero_for_nonzero`: a property test over many
/// magnitudes (from sub-micron to kilometre scale) and every unit —
/// no nonzero input should ever format to a string that reads as zero.
/// Deterministic (no new property-testing dependency for a leaf crate):
/// sweeps a dense geometric magnitude ladder combined with several
/// mantissas and every unit, rather than relying on external random
/// sampling.
#[test]
fn format_never_prints_zero_for_nonzero() {
    let mantissas = [1.0, 1.5, 2.0, 3.3, 4.9, 7.1, 9.9, 0.999_999, 1.000_001];
    let mut checked = 0usize;
    for exponent in -12..=12i32 {
        let scale = 10f64.powi(exponent);
        for &mantissa in &mantissas {
            let v = mantissa * scale;
            if v == 0.0 || !v.is_finite() {
                continue;
            }
            for unit in Unit::ALL {
                let s = format_length(v, unit);
                checked += 1;
                assert!(
                    looks_nonzero(&s),
                    "format_length({v:e}, {unit:?}) printed {s:?}, which reads as zero"
                );
                let neg = format_length(-v, unit);
                assert!(
                    looks_nonzero(&neg),
                    "format_length({:e}, {unit:?}) printed {neg:?}, which reads as zero",
                    -v
                );
            }
        }
    }
    assert!(
        checked > 1000,
        "expected a broad sweep, only checked {checked}"
    );
}

/// Whether a formatted length's numeric part is anything but all zeros
/// (ignoring the sign, the decimal point and the unit suffix): "0.000
/// mm" and "0 mm" both read as zero; "2 mm" and "0.002 mm" (the version
/// that should never be produced for a genuinely small value, since the
/// formatter is expected to switch units first) do not.
fn looks_nonzero(formatted: &str) -> bool {
    let digits_part = formatted.split(' ').next().unwrap_or(formatted);
    digits_part.chars().any(|c| c.is_ascii_digit() && c != '0')
}
