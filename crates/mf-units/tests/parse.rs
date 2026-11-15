#![allow(clippy::unwrap_used)] // tests may panic on failure

//! `parse_length` across every unit, precedence and errors (consolidation
//! plan C03, ADR-0006 rule 1b).

use mf_units::{ParseError, Unit, parse_length};

fn mm(text: &str, default: Unit) -> f64 {
    parse_length(text, default).unwrap().mm()
}

#[test]
fn parses_every_unit_symbol() {
    assert!((mm("5 mm", Unit::Millimeter) - 5.0).abs() < 1e-9);
    assert!((mm("5 cm", Unit::Millimeter) - 50.0).abs() < 1e-9);
    assert!((mm("5 m", Unit::Millimeter) - 5000.0).abs() < 1e-9);
    assert!((mm("5 in", Unit::Millimeter) - 127.0).abs() < 1e-9);
    assert!((mm("5 ft", Unit::Millimeter) - 1524.0).abs() < 1e-9);
    assert!((mm("5\"", Unit::Millimeter) - 127.0).abs() < 1e-9);
    assert!((mm("5'", Unit::Millimeter) - 1524.0).abs() < 1e-9);
    assert!((mm("5000 um", Unit::Millimeter) - 5.0).abs() < 1e-9);
    assert!((mm("5000 µm", Unit::Millimeter) - 5.0).abs() < 1e-9);
}

#[test]
fn no_space_before_unit() {
    assert!((mm("25.4mm", Unit::Millimeter) - 25.4).abs() < 1e-9);
    assert!((mm("10in", Unit::Millimeter) - 254.0).abs() < 1e-9);
}

#[test]
fn bare_number_means_default_unit() {
    let l = parse_length("10", Unit::Inch).unwrap();
    assert_eq!(l.unit, Unit::Inch);
    assert!((l.value - 10.0).abs() < 1e-9);
    assert!((mm("10", Unit::Inch) - 254.0).abs() < 1e-9);
    assert!((mm("10", Unit::Millimeter) - 10.0).abs() < 1e-9);
}

#[test]
fn mixed_units_add_correctly() {
    // 5 cm + 2 mm = 52 mm.
    assert!((mm("5 cm + 2 mm", Unit::Millimeter) - 52.0).abs() < 1e-9);
    assert!((mm("1 m - 1 cm", Unit::Millimeter) - 990.0).abs() < 1e-9);
}

#[test]
fn fractions() {
    // 1/8 in = 3.175 mm.
    assert!((mm("1/8 in", Unit::Millimeter) - 3.175).abs() < 1e-9);
    assert!((mm("1/8in", Unit::Millimeter) - 3.175).abs() < 1e-9);
    assert!((mm("3/4\"", Unit::Millimeter) - 19.05).abs() < 1e-9);
}

#[test]
fn precedence_and_parentheses() {
    // 5 mm + 2 mm * 3 = 11 mm (multiplication binds tighter).
    assert!((mm("5 mm + 2 mm * 3", Unit::Millimeter) - 11.0).abs() < 1e-9);
    // (5 mm + 2 mm) * 3 = 21 mm.
    assert!((mm("(5 mm + 2 mm) * 3", Unit::Millimeter) - 21.0).abs() < 1e-9);
    // 10 mm / 2 = 5 mm.
    assert!((mm("10 mm / 2", Unit::Millimeter) - 5.0).abs() < 1e-9);
    // A unit suffix right after a parenthesised group applies to the
    // whole group: (1 + 2) in = 3 in = 76.2 mm.
    assert!((mm("(1 + 2) in", Unit::Millimeter) - 76.2).abs() < 1e-9);
}

#[test]
fn unary_minus() {
    assert!((mm("-5 mm", Unit::Millimeter) - -5.0).abs() < 1e-9);
    assert!((mm("10 mm - -5 mm", Unit::Millimeter) - 15.0).abs() < 1e-9);
}

#[test]
fn whitespace_is_insignificant() {
    assert!((mm("  5   mm  ", Unit::Millimeter) - 5.0).abs() < 1e-9);
    assert!((mm("5mm", Unit::Millimeter) - mm("5 mm", Unit::Millimeter)).abs() < 1e-9);
}

#[test]
fn result_is_in_default_unit() {
    let l = parse_length("25.4 mm", Unit::Inch).unwrap();
    assert_eq!(l.unit, Unit::Inch);
    assert!((l.value - 1.0).abs() < 1e-9);
}

#[test]
fn errors_are_typed_not_zero() {
    assert_eq!(parse_length("", Unit::Millimeter), Err(ParseError::Empty));
    assert_eq!(
        parse_length("   ", Unit::Millimeter),
        Err(ParseError::Empty)
    );
    assert!(matches!(
        parse_length("abc", Unit::Millimeter),
        Err(ParseError::Unexpected(_))
    ));
    assert!(matches!(
        parse_length("5 xyz", Unit::Millimeter),
        Err(ParseError::BadUnit(ref s)) if s == "xyz"
    ));
    assert!(matches!(
        parse_length("5 mm +", Unit::Millimeter),
        Err(ParseError::UnexpectedEnd)
    ));
    assert!(matches!(
        parse_length("(5 mm", Unit::Millimeter),
        Err(ParseError::ExpectedCloseParen)
    ));
    assert!(matches!(
        parse_length("5 mm / 0", Unit::Millimeter),
        Err(ParseError::DivideByZero)
    ));
    assert!(matches!(
        parse_length("1/0 in", Unit::Millimeter),
        Err(ParseError::DivideByZero)
    ));
    assert!(matches!(
        parse_length("5 mm )", Unit::Millimeter),
        Err(ParseError::Unexpected(')'))
    ));
}

#[test]
fn unit_times_unit_is_an_error() {
    assert_eq!(
        parse_length("5 in * 2 in", Unit::Millimeter),
        Err(ParseError::UnitTimesUnit)
    );
}

#[test]
fn never_returns_zero_for_bad_input() {
    for bad in [
        "", "   ", "abc", "5 +", "5 mm +", "(", ")", "5 /", "5 xyz", "--",
    ] {
        let result = parse_length(bad, Unit::Millimeter);
        assert!(
            result.is_err(),
            "expected an error for {bad:?}, got {result:?}"
        );
    }
}
