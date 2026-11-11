//! The tool framework (M4 final UX review I8): which tools Properties
//! offers for a whole object, in which group and order; which kind each is
//! (a one-shot operation with parameters and Apply, or an interactive tool
//! that works in the 3D view); and what the view's legend shows.
//!
//! The spec registry says what operations *exist*; this table says what
//! people are *offered*. An operation that isn't listed here never appears
//! as a button, so a new spec (an object operation, a sculpt stroke) can't
//! show up in Properties as a raw "parameters + Apply" tool by accident.

use eframe::egui::Color32;

use crate::theme;

/// How a tool works.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    /// Parameters, then Apply: one command (Fill all holes, Compact).
    OneShot,
    /// Works in the 3D view while it's open (Transform; later Split,
    /// Sculpt). While open it gets the view's primary drag before the
    /// camera does, and its own keys; Esc leaves it.
    Interactive,
}

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

/// One tool Properties offers for a whole object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    /// The operation id (one-shot), or the tool's own id (interactive).
    pub id: &'static str,
    pub group: ToolGroup,
    pub kind: ToolKind,
    /// The key that opens it from anywhere (no modifiers).
    pub shortcut: Option<eframe::egui::Key>,
}

/// Every whole-object tool, in the order Properties lists them (grouped by
/// [`ToolGroup`], and in this order within a group). An allowlist: what
/// isn't here isn't offered.
pub const CATALOGUE: &[Entry] = &[
    Entry {
        id: crate::transform_tool::ID,
        group: ToolGroup::Modify,
        kind: ToolKind::Interactive,
        shortcut: Some(eframe::egui::Key::T),
    },
    Entry {
        id: "mesh.fill_holes",
        group: ToolGroup::Repair,
        kind: ToolKind::OneShot,
        shortcut: None,
    },
    Entry {
        id: "mesh.remove_small_components",
        group: ToolGroup::Repair,
        kind: ToolKind::OneShot,
        shortcut: None,
    },
    Entry {
        id: "mesh.compact",
        group: ToolGroup::Housekeeping,
        kind: ToolKind::OneShot,
        shortcut: None,
    },
];

/// The catalogue entry for `id`, if it's offered.
pub fn entry(id: &str) -> Option<&'static Entry> {
    CATALOGUE.iter().find(|e| e.id == id)
}

/// An interactive tool that is open. While one is, it owns the view's
/// primary drag (ahead of the camera and selection, though Alt and Shift
/// camera drags still win) and its own keys, and supplies the legend's
/// rows. Esc leaves it (after cancelling a running job, then its drag).
///
/// Phase 1 adds its tools here (Transform, then Split, Sculpt).
#[derive(Debug)]
pub enum Interactive {
    Transform(crate::transform_tool::TransformTool),
}

impl Interactive {
    /// The interactive tool `id` names, opened on `app`'s target (`None`:
    /// `id` is a one-shot tool).
    pub fn open(app: &crate::MeshFixerApp, id: &str) -> Option<Interactive> {
        // Each works on an object: with none open, there's nothing to open.
        app.active_mesh()?;
        (id == crate::transform_tool::ID)
            .then(|| Interactive::Transform(crate::transform_tool::TransformTool::default()))
    }

    /// The keys it takes while open (no modifiers), each with what it
    /// does. The app routes only these to it, before its own shortcuts;
    /// with no tool open none are taken.
    pub fn keys(&self) -> &'static [(eframe::egui::Key, ToolKey)] {
        use eframe::egui::Key;
        match self {
            Interactive::Transform(_) => &[
                (Key::W, ToolKey::Mode(mf_render::GizmoMode::Move)),
                (Key::E, ToolKey::Mode(mf_render::GizmoMode::Rotate)),
                (Key::R, ToolKey::Mode(mf_render::GizmoMode::Scale)),
            ],
        }
    }

    /// True if, while it's open, the defect and recent-repair overlays
    /// (tints, outlines, markers, hover and its tooltip) are hidden, so its
    /// own colours mean one thing (Transform's axes, D8).
    pub fn hides_defects(&self) -> bool {
        match self {
            Interactive::Transform(_) => true,
        }
    }

    /// Its catalogue id.
    pub fn id(&self) -> &'static str {
        match self {
            Interactive::Transform(_) => crate::transform_tool::ID,
        }
    }

    /// Its legend rows.
    pub fn legend(&self) -> Vec<LegendRow> {
        match self {
            Interactive::Transform(_) => transform_legend(),
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
}

/// The keys the open tool (if any) takes.
pub fn tool_keys(open: Option<&Interactive>) -> &'static [(eframe::egui::Key, ToolKey)] {
    open.map_or(&[], Interactive::keys)
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

    /// Every one-shot tool names a built-in, whole-object operation, and
    /// the list is grouped in group order.
    #[test]
    fn the_catalogue_is_grouped_and_names_real_operations() {
        for e in CATALOGUE.iter().filter(|e| e.kind == ToolKind::OneShot) {
            let spec = mf_spec::builtin::get(e.id).expect("a built-in operation");
            assert!(spec.target.whole_object && !spec.is_read_only(), "{}", e.id);
        }
        assert!(CATALOGUE.windows(2).all(|w| w[0].group <= w[1].group));
    }

    /// Keys go to the open tool only: with none open, W, E and R are
    /// nobody's (review I3).
    #[test]
    fn only_an_open_tool_takes_keys() {
        assert!(tool_keys(None).is_empty());
        let t = Interactive::Transform(crate::transform_tool::TransformTool::default());
        let keys: Vec<_> = tool_keys(Some(&t)).iter().map(|(k, _)| *k).collect();
        use eframe::egui::Key;
        assert_eq!(keys, [Key::W, Key::E, Key::R]);
    }

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
