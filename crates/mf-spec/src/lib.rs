//! Declarative operation specifications (MF-SPEC-001).
//!
//! Each operation is defined once in `specs/operations/<id>.yaml`. GUI
//! controls, CLI arguments and validation are all derived from that
//! definition (MF-SPEC-003).

pub mod builtin;
pub mod cli;
mod model;
mod params;

pub use model::{
    Coalesce, FitDefaults, OperandRole, OperandSpec, OperationSpec, ParamKind, ParamSpec,
    ParamUnit, RefusalArgKind, RefusalArgSpec, RefusalNext, RefusalSpec, RuntimeSpec, TargetSpec,
};
pub use params::{FaceList, ParamValue, Params, SpecError};

/// A length in document units, to three significant figures: never
/// "0.000" the way a plain `{:.3}` would print a sub-millimetre value
/// on a metre document (the owner's bug — Make solid's refusal saying
/// "at least 0.000" for a genuinely positive cell size). No unit
/// suffix: operations never see which unit the document is in (D20
/// keeps them working purely in document units), so a message built
/// from this reads "at least 0.00025" — the frontend's own words
/// around it name the unit, as every other length in its panels does.
///
/// Re-exported from `mf-units` (consolidation plan C05), fixing the
/// number of significant figures at 3 (ADR-0006 rule 4): the same
/// unitless core every length/area/volume formatter in the app shares
/// (`mf_units::significant_figures`), called with no `Unit` because
/// this function's own contract (D20) is that it never has one.
pub fn format_length(v: f64) -> String {
    mf_units::significant_figures(v, 3)
}
