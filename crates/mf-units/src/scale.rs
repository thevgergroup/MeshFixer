//! [`ScaleCtx`]: the small bundle of scale facts a panel or tool needs
//! to show and snap lengths correctly — the document's unit, the
//! display unit a person sees, how big the model is, and (while
//! interacting) how many world millimetres one screen point covers.
//! Nothing here does unit conversion math directly; it's the context
//! other `mf-units` functions ([`crate::nice_step`], [`crate::format_length`])
//! are driven by.

use crate::unit::Unit;

/// Scale context for a document and its current view. `world_per_pt` is
/// `None` when there is no live camera (e.g. computing a default before
/// a view exists); callers of [`crate::nice_step`] should treat that as
/// "use the document scale with no zoom adaptation" rather than
/// guessing a camera distance.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScaleCtx {
    /// The document's own unit (what recorded/geometry values are in).
    pub doc: Unit,
    /// The unit shown to the person (may differ from `doc`, e.g. a
    /// millimetre document displayed in inches).
    pub display: Unit,
    /// The model's bounding-box diagonal, in document units. Used for
    /// bbox-relative defaults (ADR-0006 §6.4: cell sizes, bound sizes),
    /// not by anything in this module directly.
    pub bbox_diag: f64,
    /// How many document units of world space one screen point covers
    /// at the current zoom, if a camera/view is live.
    pub world_per_pt: Option<f64>,
}

impl ScaleCtx {
    /// A `ScaleCtx` with `display` the same as `doc`, no live view and a
    /// zero bbox — a reasonable starting point before a document or
    /// camera exists.
    pub fn new(doc: Unit) -> ScaleCtx {
        ScaleCtx {
            doc,
            display: doc,
            bbox_diag: 0.0,
            world_per_pt: None,
        }
    }

    /// `world_per_pt` converted to millimetres per point, for callers of
    /// [`crate::nice_step`], which works in mm internally. Falls back to
    /// the document unit's own mm-per-unit when there is no live camera,
    /// so a step can still be chosen (just without zoom adaptation).
    pub fn mm_per_pt(&self) -> f64 {
        match self.world_per_pt {
            Some(doc_units_per_pt) => doc_units_per_pt * self.doc.mm_per(),
            None => self.doc.mm_per(),
        }
    }
}
