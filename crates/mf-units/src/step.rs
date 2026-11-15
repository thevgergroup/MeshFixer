//! [`nice_step`]: the one 1-2-5 snap/grid step used by every gizmo tool
//! and grid (ADR-0006 rule 3). A fixed step in document units snaps a
//! gizmo to zero on a metre-scale model (1 document unit = 1000 mm) and
//! is unusably coarse on a micron-scale one; `nice_step` instead picks
//! the finest step from a 1-2-5 sequence whose on-screen size still
//! clears a minimum (by default 8 pt), at whatever zoom and whatever
//! document unit.

use crate::unit::Unit;

/// The 1-2-5 sequence a step is chosen from, in millimetres, spanning a
/// micron-model floor to a whole-building ceiling — wide enough that
/// [`nice_step`] always finds a step clearing `min_pt`, however far
/// zoomed in or out, short of the sequence's own extremes.
const STEP_SEQUENCE_MM: [f64; 20] = [
    0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0,
    200.0, 500.0, 1000.0, 5000.0,
];

/// Picks a step from the 1-2-5 sequence, in `display` units, whose
/// on-screen size is at least `min_pt` points, given `world_per_pt`
/// (how many millimetres of world space one screen point covers at the
/// current zoom — the caller's own camera/projection math produces
/// this; `nice_step` itself has no notion of a camera).
///
/// Falls back to the sequence's own largest step if even that still
/// projects under `min_pt` (an extreme zoom-out): snapping then does
/// less than asked rather than silently vanishing to an unusable step.
pub fn nice_step(world_per_pt: f64, min_pt: f64, display: Unit) -> crate::length::Length {
    let mm_per_pt = world_per_pt.max(1e-12);
    let step_mm = STEP_SEQUENCE_MM
        .iter()
        .copied()
        .find(|&mm| mm / mm_per_pt >= min_pt)
        .unwrap_or(STEP_SEQUENCE_MM[STEP_SEQUENCE_MM.len() - 1]);
    crate::length::Length::new(step_mm / display.mm_per(), display)
}
