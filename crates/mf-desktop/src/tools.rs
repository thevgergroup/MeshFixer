//! Shared tool vocabulary: the groups Repair lists its operations in
//! (`tool_panels::REPAIRS` is the curated list), what a tool's key does,
//! and what the view's legend shows. Tools themselves are `shell::Tool`s
//! with a `session::ToolSession` (ADR-0007).

use eframe::egui::Color32;

use crate::theme;

/// The groups Properties lists whole-object tools in, in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ToolGroup {
    /// Change the object's placement or shape (Transform; later Split,
    /// Sculpt).
    Modify,
    /// Fix what the analysis found.
    Repair,
    /// Several objects into one (booleans; later Parts).
    Combine,
    /// Upkeep with no visible effect (Compact).
    Housekeeping,
}

impl ToolGroup {
    /// The group's heading in Properties.
    pub fn label(self) -> &'static str {
        match self {
            ToolGroup::Modify => "Modify",
            ToolGroup::Repair => "Repair",
            ToolGroup::Combine => "Combine",
            ToolGroup::Housekeeping => "Housekeeping",
        }
    }
}

/// What a tool's key does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKey {
    /// Transform: switch the gizmo's mode.
    Mode(mf_render::GizmoMode),
    /// Sculpt: switch the active brush.
    Brush(mf_operations::sculpt::BrushKind),
    /// Connect: remove the picked connector (Delete, Backspace). A tool
    /// that takes this key owns it while open: the app's own Delete
    /// (`object.delete`) never runs instead (owner 2026-11-11).
    DeleteSelection,
}

/// The legend while Transform is open (D8): the gizmo's axis colours, in
/// place of the defect colours it hides.
pub fn transform_legend() -> Vec<LegendRow> {
    let row = |name, color, meaning| LegendRow {
        name,
        color,
        meaning,
        switch: None,
    };
    vec![
        row(
            "X axis",
            theme::AXIS_X,
            "The gizmo's X handles: drag the arrow to move along X, the ring or its curved \
             arrows to turn about X",
        ),
        row(
            "Y axis",
            theme::AXIS_Y,
            "The gizmo's Y handles: drag the arrow to move along Y, the ring or its curved \
             arrows to turn about Y",
        ),
        row(
            "Z axis",
            theme::AXIS_Z,
            "The gizmo's Z handles: drag the arrow to move along Z, the ring or its curved \
             arrows to turn about Z",
        ),
        row(
            "Free handles",
            theme::GIZMO_FREE,
            "The centre (move in the view's plane), the ring that appears just outside the \
             others when you point there (turn in the view's plane) and the centre of the \
             scale handles (scale evenly)",
        ),
        LegendRow {
            name: "Hover",
            color: theme::HOVER,
            meaning: "What the pointer is over, in the view or the tree",
            switch: Some(Switch::Hover),
        },
    ]
}

/// Which overlay switch a legend row toggles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Switch {
    Selection,
    Hover,
    Holes,
    HoleMarkers,
    Cracks,
    LoosePieces,
    Tangled,
    RecentRepairs,
}

impl Switch {
    /// True if this overlay is a defect's (View > Show defects hides them).
    pub fn is_defect(self) -> bool {
        matches!(
            self,
            Switch::Holes
                | Switch::HoleMarkers
                | Switch::Cracks
                | Switch::LoosePieces
                | Switch::Tangled
        )
    }
}

/// One row of the view's legend: a swatch, a name, what it means and,
/// for an overlay that can be hidden, its switch.
#[derive(Debug, Clone, PartialEq)]
pub struct LegendRow {
    pub name: &'static str,
    pub color: Color32,
    pub meaning: &'static str,
    pub switch: Option<Switch>,
}

/// The legend while no interactive tool supplies its own: what each
/// overlay colour in the view means.
pub fn default_legend() -> Vec<LegendRow> {
    let row = |name, color, meaning, switch| LegendRow {
        name,
        color,
        meaning,
        switch,
    };
    vec![
        row(
            "Selection",
            theme::SELECTED,
            "The current target: outlined, or tinted; shown faintly through whatever hides it",
            Some(Switch::Selection),
        ),
        row(
            "Hover",
            theme::HOVER,
            "What the pointer is over, in the view or the tree",
            Some(Switch::Hover),
        ),
        row(
            "Holes",
            theme::HOLE,
            "Open boundaries, outlined",
            Some(Switch::Holes),
        ),
        row(
            "Hole markers",
            theme::HOLE,
            "A dot on each hole too small to see at this zoom (behind the model: none)",
            Some(Switch::HoleMarkers),
        ),
        row(
            "Narrow cracks",
            theme::CRACK,
            "Boundaries whose edges almost touch",
            Some(Switch::Cracks),
        ),
        row(
            "Loose pieces",
            theme::LOOSE_PIECE,
            "Pieces not connected to the model (every one the tree lists), tinted; click one to \
             select it",
            Some(Switch::LoosePieces),
        ),
        row(
            "Tangled edges",
            theme::NON_MANIFOLD,
            "Tangled (non-manifold) edges: more than two triangles share each one. The triangles \
             at them are tinted; click one to select its faces",
            Some(Switch::Tangled),
        ),
        row(
            "Recent repairs",
            theme::RECENT_REPAIR,
            "Faces made by the last 3 commands, tinted",
            Some(Switch::RecentRepairs),
        ),
        // Shown with the holes (never hidden alone: the hole colour is
        // under it).
        row(
            "Intentional openings",
            theme::OPENING,
            "Openings you marked as meant to be open, outlined; shown and hidden with Holes",
            None,
        ),
    ]
}

/// The left panel's sections. Objects (the tree) today; the Parts library
/// (Phase 1 Task 23) joins it, and then the panel shows a tab for each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LeftTab {
    #[default]
    Objects,
}

impl LeftTab {
    /// Every section, in tab order.
    pub const ALL: &[LeftTab] = &[LeftTab::Objects];

    pub fn label(self) -> &'static str {
        match self {
            LeftTab::Objects => "Objects",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each colour in the default legend means one thing (the hole colour
    /// twice: the outline and its markers are both holes).
    #[test]
    fn the_default_legend_says_what_each_colour_means() {
        let rows = default_legend();
        for r in &rows {
            let same: Vec<_> = rows.iter().filter(|o| o.color == r.color).collect();
            assert!(
                same.len() == 1 || same.iter().all(|o| o.color == theme::HOLE),
                "{}",
                r.name
            );
        }
    }
}
