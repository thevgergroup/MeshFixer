//! The exact fit check (owner requirements 2026-11-11): after a connector
//! or peg is added, measure the actual gap between the mating surfaces
//! exactly (`mf_geometry::minimum_distance`, the BVH minimum distance),
//! and report it against the chosen fit class and machine profile in
//! plain language. Never assumed: this always measures the meshes as
//! they are, not the parameters that were meant to produce them.

use glam::DMat4;
use mf_geometry::{Bvh, Mesh};
use mf_runtime::{Cancelled, RunContext};

use crate::fit::{Compensation, FitClass};
use crate::machine::MachineProfile;

/// The result of measuring one mating pair against a fit class.
#[derive(Debug, Clone, PartialEq)]
pub struct FitCheck {
    /// What the two surfaces are called in the message (e.g. "Hinge pin
    /// to knuckle").
    pub pair: String,
    /// The exact gap measured on the meshes, mm. Zero if they touch or
    /// overlap (never negative: see `minimum_distance`).
    pub measured_gap: f64,
    /// What this fit class, on this machine, expects (the resolved
    /// value, `fit::resolve`).
    pub expected_gap: f64,
    pub fit_class: String,
    pub machine: String,
    /// True when the measured gap is enough below what's expected that
    /// the parts risk fusing (printed as one solid): `measured_gap` is
    /// less than half of `expected_gap` (and `expected_gap` is
    /// positive; a press fit's negative expectation is never "too
    /// tight" by this check, that's its point).
    pub may_fuse: bool,
}

impl FitCheck {
    /// The plain-language message (the owner's example: "Hinge pin to
    /// knuckle: 0.15 mm gap. Your Snug fit on 'Prusa MK4' expects 0.3
    /// mm. It may fuse; increase the clearance or choose Sliding.").
    pub fn message(&self) -> String {
        let mut s = format!(
            "{}: {:.2} mm gap. Your {} fit on '{}' expects {:.2} mm.",
            self.pair, self.measured_gap, self.fit_class, self.machine, self.expected_gap
        );
        if self.may_fuse {
            s.push_str(" It may fuse; increase the clearance or choose a looser fit.");
        }
        s
    }
}

/// The two mating surfaces to measure (in `a`'s frame; `b` placed by
/// `b_to_a`).
pub struct MatingSurfaces<'a> {
    pub a: &'a Mesh,
    pub a_bvh: &'a Bvh,
    pub b: &'a Mesh,
    pub b_to_a: &'a DMat4,
}

/// What to measure the mating surfaces against: a fit class resolved
/// through a machine profile's relevant tolerance.
pub struct FitExpectation<'a> {
    pub fit_class: &'a FitClass,
    pub machine: &'a MachineProfile,
    pub compensation: Compensation,
}

/// Measures the gap between `surfaces.a` and `surfaces.b`, and reports it
/// against `expect`'s resolved value. `pair` names the mating surfaces
/// for the message (owner's example: "Hinge pin to knuckle").
pub fn check_fit(
    pair: &str,
    surfaces: MatingSurfaces,
    expect: FitExpectation,
    ctx: &RunContext,
) -> Result<Option<FitCheck>, Cancelled> {
    let MatingSurfaces {
        a,
        a_bvh,
        b,
        b_to_a,
    } = surfaces;
    let Some(measured_gap) = mf_geometry::minimum_distance(a, a_bvh, b, b_to_a, ctx)? else {
        return Ok(None);
    };
    let FitExpectation {
        fit_class,
        machine,
        compensation,
    } = expect;
    let expected_gap = crate::fit::resolve(fit_class, machine, compensation);
    let may_fuse = expected_gap > 0.0 && measured_gap < expected_gap * 0.5;
    Ok(Some(FitCheck {
        pair: pair.to_string(),
        measured_gap,
        expected_gap,
        fit_class: fit_class.name.clone(),
        machine: machine.name.clone(),
        may_fuse,
    }))
}
