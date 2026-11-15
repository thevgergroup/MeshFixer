//! Units, lengths, parsing, formatting and scale (ADR-0006, consolidation
//! plan C03). A leaf crate: no `mf-*` dependencies. Every other crate
//! that needs a unit, a length or a display format depends on this one,
//! never on each other's copy.
//!
//! - [`Unit`]: the six length units MeshFixer knows, and the one
//!   mm-per-unit table.
//! - [`Length`] / [`DocLength`]: a value with its unit, and a marker for
//!   "already in document units".
//! - [`parse_length`]: the expression parser shared by every length
//!   field, the CLI and specs.
//! - [`format_length`], [`format_area`], [`format_volume`]: the one
//!   formatter, everywhere a length/area/volume is shown.
//! - [`nice_step`]: the 1-2-5 snap/grid step.
//! - [`ScaleCtx`]: the scale facts a panel needs to show and snap
//!   correctly.

mod format;
mod length;
mod parse;
mod scale;
mod step;
mod unit;

pub use format::{format_area, format_length, format_volume, significant_figures};
pub use length::{DocLength, Length};
pub use parse::{ParseError, parse_length};
pub use scale::ScaleCtx;
pub use step::nice_step;
pub use unit::Unit;
