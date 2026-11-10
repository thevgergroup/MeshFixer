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
    OperandRole, OperandSpec, OperationSpec, ParamKind, ParamSpec, RuntimeSpec, TargetSpec,
};
pub use params::{ParamValue, Params, SpecError};
