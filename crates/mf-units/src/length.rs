//! [`Length`] (a value with a unit, for anything a person types or
//! reads) and [`DocLength`] (a newtype marking "already in document
//! units", for anything an op receives). Keeping these as distinct types
//! is ADR-0006 rule 1: tool code never passes a bare `f64` as a length.

use crate::unit::Unit;

/// A length with its unit, exactly as typed or displayed: "10 in" is
/// `Length { value: 10.0, unit: Unit::Inch }`, not 254.0 of anything.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Length {
    pub value: f64,
    pub unit: Unit,
}

impl Length {
    pub const fn new(value: f64, unit: Unit) -> Length {
        Length { value, unit }
    }

    /// This length in millimetres.
    pub fn mm(self) -> f64 {
        self.value * self.unit.mm_per()
    }

    /// This length converted to `unit`, keeping the same physical size.
    pub fn to(self, unit: Unit) -> Length {
        Length {
            value: self.mm() / unit.mm_per(),
            unit,
        }
    }

    /// This length's value once converted into document units, given the
    /// document's own [`Unit`] — the one conversion `Session::prepare`
    /// performs (ADR-0006 rule 2). The result is a bare `f64` because
    /// that's what ops store; wrap it in [`DocLength`] at the boundary
    /// where that distinction matters.
    pub fn in_doc(self, doc: Unit) -> f64 {
        self.mm() / doc.mm_per()
    }
}

/// A value already expressed in document units: the frontier past which
/// nothing converts again. Ops and generators that receive a plain `f64`
/// length should receive a `DocLength` instead, so a reviewer can tell at
/// the call site that no further unit conversion is needed (or missing).
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct DocLength(pub f64);

impl DocLength {
    /// This document-unit value's length, given the document's unit.
    pub fn length(self, doc: Unit) -> Length {
        Length::new(self.0, doc)
    }
}

impl From<f64> for DocLength {
    fn from(value: f64) -> DocLength {
        DocLength(value)
    }
}

impl From<DocLength> for f64 {
    fn from(value: DocLength) -> f64 {
        value.0
    }
}
