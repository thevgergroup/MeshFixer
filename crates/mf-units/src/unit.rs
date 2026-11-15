//! [`Unit`]: the length units MeshFixer understands, and the one table of
//! how many millimetres each is worth (ADR-0006). This used to live in
//! `mf-io::threemf`, next to the 3MF `unit` attribute it was first added
//! for; it moved here so every crate that needs a unit (not just 3MF
//! import/export) can depend on a leaf with no mf-* dependencies of its
//! own. `mf-io` re-exports it (`pub use mf_units::Unit`) and keeps the 3MF
//! attribute string mapping, which is 3MF's concern, not a unit's own.

use serde::{Deserialize, Serialize};

/// A length unit. `mm_per` is the single source of truth for every
/// conversion in the app: never multiply or divide by a literal
/// millimetre-per-unit constant anywhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Unit {
    Micron,
    #[default]
    Millimeter,
    Centimeter,
    Meter,
    Inch,
    Foot,
}

impl Unit {
    /// Every unit, in a stable, display-friendly order (smallest to
    /// largest within each system): metric then imperial. Used to build
    /// unit pickers (the import dialog's `Unit::ALL`, ADR-0006 "still to
    /// do: ... offering only 4 units").
    pub const ALL: [Unit; 6] = [
        Unit::Micron,
        Unit::Millimeter,
        Unit::Centimeter,
        Unit::Meter,
        Unit::Inch,
        Unit::Foot,
    ];

    /// How many of this unit make one millimetre. For example, 1 m is
    /// 1000 mm, so `Unit::Meter.mm_per() == 1000.0`. A length `v` in this
    /// unit is therefore `v * self.mm_per()` millimetres, and a length in
    /// mm is `mm / self.mm_per()` of this unit.
    pub const fn mm_per(self) -> f64 {
        match self {
            Unit::Micron => 0.001,
            Unit::Millimeter => 1.0,
            Unit::Centimeter => 10.0,
            Unit::Meter => 1000.0,
            Unit::Inch => 25.4,
            Unit::Foot => 304.8,
        }
    }

    /// The short symbol used in fields, status text and formatted output:
    /// "mm", "m", "in", and so on.
    pub const fn symbol(self) -> &'static str {
        match self {
            Unit::Micron => "µm",
            Unit::Millimeter => "mm",
            Unit::Centimeter => "cm",
            Unit::Meter => "m",
            Unit::Inch => "in",
            Unit::Foot => "ft",
        }
    }

    /// The full English name, for pickers and tooltips: "millimetre",
    /// "inch", and so on.
    pub const fn name(self) -> &'static str {
        match self {
            Unit::Micron => "micrometre",
            Unit::Millimeter => "millimetre",
            Unit::Centimeter => "centimetre",
            Unit::Meter => "metre",
            Unit::Inch => "inch",
            Unit::Foot => "foot",
        }
    }

    /// Parses a unit symbol: any of `mm`, `cm`, `m`, `in`, `ft`, `"`, `'`,
    /// plus the micron spellings `µm` and `um`. Case-sensitive except
    /// where the symbol is inherently unambiguous either way (there is
    /// none here: 3MF and every panel use lower-case symbols, so this
    /// only accepts those plus the two ASCII quote characters). Returns
    /// `None` on anything else; callers turn that into a named parse
    /// error rather than defaulting silently.
    pub fn parse_symbol(s: &str) -> Option<Unit> {
        Some(match s {
            "mm" => Unit::Millimeter,
            "cm" => Unit::Centimeter,
            "m" => Unit::Meter,
            "in" | "\"" => Unit::Inch,
            "ft" | "'" => Unit::Foot,
            "µm" | "um" => Unit::Micron,
            _ => return None,
        })
    }
}
